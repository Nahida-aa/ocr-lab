//! 基于 xdg-desktop-portal **ScreenCast** + PipeWire 的抓图后端。
//!
//! 与 `PortalCapturer`（Screenshot 接口）的区别：
//! - Screenshot 只能抓「合成后的全屏」，会被遮挡影响，且无法指定某个窗口。
//! - ScreenCast 可以提供「选窗口」能力，返回的是**该窗口自身的合成流**，
//!   **不受遮挡影响** —— 这正是录屏软件「选 app」的来源。底层用 PipeWire
//!   消费流，按需抽帧（理论上可持续录屏、随时抽帧）。
//!
//! 两种输入（对应需求里的两类）：
//! 1. 全屏：`capture_fullscreen` → `SourceType::Monitor`（受遮挡影响，符合全屏语义）。
//! 2. 应用窗口：`capture_app(restore_token)` → `SourceType::Window`（窗口本体，不受遮挡）。
//!
//! 关于「提前赋权」：portal 的窗口选择不是按 app 名字直接选，而是首次选择后
//! 由 compositor 返回一个 `restore_token`；之后把这个 token 传回去即可免对话框
//! 自动恢复同一窗口选择。所以 `capture_app` 接收的是 `restore_token`。
//!
//! 验证状态：已在 KDE/Wayland 实测 —— 选窗 + 抽帧成功，协商格式 BGRA，
//! B/R 通道交换正确，restore_token 可复用。

use anyhow::{Context, Result};
use ashpd::desktop::PersistMode;
use ashpd::desktop::screencast::{Screencast, SelectSourcesOptions, SourceType};
use enumflags2::BitFlags;
use image::RgbaImage;
use pipewire as pw;
use pipewire::stream::StreamFlags;
use pw::spa::buffer::DataType;
use pw::spa::param::format::{MediaSubtype, MediaType};
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use pw::spa::pod::Pod;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 跨 PipeWire 回调共享的状态。
struct FrameState {
    /// 协商后的视频格式（含实际 size / format）。
    info: VideoInfoRaw,
    /// 抽到的帧：RGBA 像素（已做 BGRA→RGBA 交换）。
    frame: Option<RgbaImage>,
    done: AtomicBool,
}

/// 通过 ScreenCast portal 选源，拿到 PipeWire 的 node id 与 remote fd。
///
/// - `source`：要捕获的源类型（Monitor 全屏 / Window 窗口）。
/// - `restore_token`：上一次选择得到的 token，传回可免对话框恢复同一选择。
///
/// 返回 `(node_id, fd, 本次返回的 restore_token, 窗口在屏幕上的位置)`。
/// 其中位置 `(x, y)` 来自 portal 响应的 `Stream::position()`，是窗口左上角的
/// 屏幕绝对坐标（compositor 坐标系，含分数缩放）。Capture 窗口流时可用它把
/// 「窗口相对坐标」换算成「屏幕绝对坐标」去点击，无需再查 compositor。
async fn select_stream(
    source: SourceType,
    restore_token: Option<&str>,
) -> Result<(u32, OwnedFd, Option<String>, Option<(i32, i32)>)> {
    let sc = Screencast::new()
        .await
        .context("创建 Screencast 代理失败")?;
    let session = sc
        .create_session(Default::default())
        .await
        .context("create_session 失败")?;

    let mut opts = SelectSourcesOptions::default()
        .set_sources(BitFlags::from(source))
        .set_multiple(false)
        .set_persist_mode(PersistMode::ExplicitlyRevoked);
    if let Some(token) = restore_token {
        opts = opts.set_restore_token(token);
    }

    sc.select_sources(&session, opts)
        .await
        .context("select_sources 失败（可能需在桌面环境中授权）")?;

    let req = sc
        .start(&session, None, Default::default())
        .await
        .context("start 失败")?;
    let streams = req
        .response()
        .context("等待 start 响应失败（可能需要在桌面环境中授权）")?;

    let new_token = streams.restore_token().map(str::to_string);
    let stream = streams.streams().first().context("start 未返回任何流")?;
    let node_id = stream.pipe_wire_node_id();
    let position = stream.position();

    let fd = sc
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .context("open_pipe_wire_remote 失败")?;

    Ok((node_id, fd, new_token, position))
}

/// 连上 PipeWire 的 remote fd，协商视频格式，抽一帧返回 RGBA 图。
fn extract_one_frame(node_id: u32, fd: OwnedFd) -> Result<RgbaImage> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None).context("创建 PipeWire MainLoop 失败")?;
    let context =
        pw::context::ContextRc::new(&mainloop, None).context("创建 PipeWire Context 失败")?;
    let core = context
        .connect_fd_rc(fd, None)
        .context("连接 PipeWire remote fd 失败")?;

    let state = Arc::new(Mutex::new(FrameState {
        info: VideoInfoRaw::new(),
        frame: None,
        done: AtomicBool::new(false),
    }));
    // 监听器会拿走 state 的所有权，另外保留一份 Arc 供抽帧后读取结果。
    let state_for_listener = state.clone();

    let stream = pw::stream::StreamBox::new(
        &core,
        "capturer-screencast",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .context("创建 Stream 失败")?;

    // MainLoopRc 是引用计数的，克隆一份给 process 回调用于退出主循环，
    // 原 mainloop 仍由本函数持有以调用 run()。
    let mainloop_quit = mainloop.clone();

    let _listener = stream
        .add_local_listener_with_user_data(state_for_listener)
        .state_changed(|_stream, _user_data, _old, _new| {
            // 仅用于调试；正常流程不需要打印。
        })
        .param_changed(|_stream, user_data, id, param| {
            let Some(param) = param else { return };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let (media_type, media_subtype) =
                match pw::spa::param::format_utils::parse_format(param) {
                    Ok(v) => v,
                    Err(_) => return,
                };
            if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
                return;
            }
            let mut st = user_data.lock().unwrap();
            if let Err(e) = st.info.parse(param) {
                eprintln!("解析视频格式失败: {:?}", e);
            }
        })
        .process(move |stream, user_data| {
            let done = { user_data.lock().unwrap().done.load(Ordering::SeqCst) };
            if done {
                return;
            }
            let Some(mut buf) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buf.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let chunk = data.chunk();
            let size = chunk.size() as usize;
            let stride = chunk.stride() as usize;
            let offset = chunk.offset() as usize;
            if size == 0 || stride == 0 {
                return;
            }

            // 取像素指针：优先 pipewire 已 map 的内存；否则对 MemFd 手动 mmap。
            let data_type = data.type_();
            let fd_raw = data.fd();
            let ptr: *const u8 = if let Some(slice) = data.data() {
                slice.as_ptr()
            } else if data_type == DataType::MemFd {
                let borrowed = unsafe { BorrowedFd::borrow_raw(fd_raw) };
                let map_len = (offset + size).max(1);
                let map_len = NonZeroUsize::new(map_len).unwrap();
                match unsafe {
                    nix::sys::mman::mmap(
                        None,
                        map_len,
                        nix::sys::mman::ProtFlags::PROT_READ,
                        nix::sys::mman::MapFlags::MAP_PRIVATE,
                        borrowed,
                        offset as i64,
                    )
                } {
                    Ok(p) => unsafe { p.as_ptr().add(offset) as *const u8 },
                    Err(e) => {
                        eprintln!("mmap MemFd 失败: {:?}", e);
                        return;
                    }
                }
            } else {
                eprintln!("不支持的 buffer 类型: {:?}", data_type);
                return;
            };

            let (width, height, is_bgra) = {
                let st = user_data.lock().unwrap();
                let info = &st.info;
                let width = info.size().width;
                let height = info.size().height;
                let is_bgra = matches!(info.format(), VideoFormat::BGRA | VideoFormat::BGRx);
                (width, height, is_bgra)
            };
            if width == 0 || height == 0 {
                eprintln!("帧尺寸未知，跳过");
                return;
            }

            // 按 stride 拷贝并做 B/R 通道交换（BGRA/BGRx → RGBA）。
            let mut rgba = vec![0u8; width as usize * height as usize * 4];
            unsafe {
                let src = std::slice::from_raw_parts(ptr, stride * height as usize);
                for y in 0..height as usize {
                    let row = &src[y * stride..];
                    let dst = &mut rgba[y * width as usize * 4..];
                    for x in 0..width as usize {
                        let s = x * 4;
                        let d = x * 4;
                        if is_bgra {
                            dst[d] = row[s + 2];
                            dst[d + 1] = row[s + 1];
                            dst[d + 2] = row[s];
                            dst[d + 3] = row[s + 3];
                        } else {
                            dst[d] = row[s];
                            dst[d + 1] = row[s + 1];
                            dst[d + 2] = row[s + 2];
                            dst[d + 3] = row[s + 3];
                        }
                    }
                }
            }

            if let Some(img) = RgbaImage::from_raw(width, height, rgba) {
                let mut st = user_data.lock().unwrap();
                st.frame = Some(img);
                st.done.store(true, Ordering::SeqCst);
                mainloop_quit.quit();
            } else {
                eprintln!("构造 RgbaImage 失败");
            }
        })
        .register()
        .context("注册 stream listener 失败")?;

    // 协商视频格式：RGBA/BGRA 候选，尺寸范围。
    let obj = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            MediaType::Video
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            MediaSubtype::Raw
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::RGBA,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::BGRx
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pw::spa::utils::Rectangle {
                width: 320,
                height: 240
            },
            pw::spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            pw::spa::utils::Rectangle {
                width: 4096,
                height: 4096
            }
        )
    );

    let serialized = pw::spa::pod::serialize::PodSerializer::serialize(
        Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow::anyhow!("序列化格式 pod 失败: {:?}", e))?
    .0
    .into_inner();
    let mut params =
        [Pod::from_bytes(&serialized).ok_or_else(|| anyhow::anyhow!("解析 pod 失败"))?];

    stream
        .connect(
            pw::spa::utils::Direction::Input,
            Some(node_id),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .context("stream connect 失败")?;

    mainloop.run();

    let state = state.lock().unwrap();
    match &state.frame {
        Some(img) => Ok(img.clone()),
        None => anyhow::bail!("未抽到任何帧（portal 未授权 / 窗口选择被取消？）"),
    }
}

/// 基于 xdg-desktop-portal **ScreenCast** + PipeWire 的后端。
///
/// 支持「全屏」与「指定窗口（不受遮挡）」两类输入。两类选择各用各自的
/// `restore_token` 持久化实现「提前赋权」：**全屏 token 只用于全屏、窗口 token
/// 只用于窗口**（二者是不同源，不能串用，否则 portal 不认、照样弹窗）。
/// `PersistMode` 已设为 `Persistent`，故点一次授权后 token 长期有效，无需再弹。
pub struct ScreenCastCapturer {
    /// 全屏（Monitor 源）选择的 restore_token。
    token_monitor: Option<String>,
    /// 窗口（Window 源）选择的 restore_token。
    token_window: Option<String>,
}

impl ScreenCastCapturer {
    /// 新建（无 token，首次两类选择都会弹对话框）。
    pub fn new() -> Self {
        Self {
            token_monitor: None,
            token_window: None,
        }
    }

    /// 用已有的窗口 token 构造（兼容旧用法），下次 `capture_app` 免对话框。
    pub fn with_restore_token(token: impl Into<String>) -> Self {
        Self {
            token_monitor: None,
            token_window: Some(token.into()),
        }
    }

    /// 分别设置全屏 / 窗口 token（推荐：两类各自复用，不串用）。
    pub fn with_tokens(monitor: impl Into<String>, window: impl Into<String>) -> Self {
        Self {
            token_monitor: Some(monitor.into()),
            token_window: Some(window.into()),
        }
    }

    /// 单独设置窗口 token。
    pub fn with_window_token(token: impl Into<String>) -> Self {
        Self {
            token_monitor: None,
            token_window: Some(token.into()),
        }
    }

    /// 单独设置全屏 token。
    pub fn with_monitor_token(token: impl Into<String>) -> Self {
        Self {
            token_monitor: Some(token.into()),
            token_window: None,
        }
    }

    /// 返回内部持有的窗口 token（若有）。
    pub fn restore_token(&self) -> Option<&str> {
        self.token_window.as_deref()
    }

    /// 返回内部持有的全屏 token（若有）。
    pub fn restore_token_monitor(&self) -> Option<&str> {
        self.token_monitor.as_deref()
    }

    /// 返回内部持有的窗口 token（若有）。
    pub fn restore_token_window(&self) -> Option<&str> {
        self.token_window.as_deref()
    }
}

impl Default for ScreenCastCapturer {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::Capturer for ScreenCastCapturer {
    async fn capture_fullscreen(&self) -> Result<RgbaImage> {
        // 复用已持久化的全屏 token（若有），避免每次抓全屏都弹选屏对话框。
        let token = self.token_monitor.as_deref();
        let (node_id, fd, _token, _pos) = select_stream(SourceType::Monitor, token).await?;
        extract_one_frame(node_id, fd)
    }
}

impl ScreenCastCapturer {
    /// 同 `Capturer::capture_fullscreen`（Monitor 源，坐标即屏幕绝对坐标），但额外
    /// 返回本次（可能新生成的）全屏 `restore_token`，便于调用方持久化实现「提前赋权」。
    pub async fn capture_fullscreen_token(&self) -> Result<(RgbaImage, Option<String>)> {
        let (node_id, fd, token, _pos) =
            select_stream(SourceType::Monitor, self.token_monitor.as_deref()).await?;
        let img = extract_one_frame(node_id, fd)?;
        Ok((img, token))
    }
}

impl ScreenCastCapturer {
    /// 捕获指定窗口（窗口本体流，不受遮挡）。
    ///
    /// `app_id` 在此后端里即 portal 的窗口 restore_token：portal 不支持按 app 名字
    /// 直接选窗，而是用首次选择得到的 token 复选。调用方应传入窗口 token（或构造时
    /// `with_window_token` 设置的值）；传空串则回退到内部持有的窗口 token。若都没有，
    /// 会弹对话框让你选窗。
    pub async fn capture_app(&self, app_id: &str) -> Result<RgbaImage> {
        let (img, _pos, _tok) = self.capture_app_geom(app_id).await?;
        Ok(img)
    }

    /// 同 `capture_app`，但额外返回窗口在屏幕上的位置 `(x, y)`（来自 portal 响应
    /// 的 `Stream::position()`，compositor 坐标系）。可用它把 OCR 得到的「窗口相对
    /// 坐标」换算成「屏幕绝对坐标」去点击。无位置信息时为 `None`（如 Monitor 源）。
    ///
    /// 返回的第三项是本次（可能新生成的）窗口 restore_token，便于调用方持久化。
    pub async fn capture_app_geom(
        &self,
        app_id: &str,
    ) -> Result<(RgbaImage, Option<(i32, i32)>, Option<String>)> {
        let token = if app_id.is_empty() {
            self.token_window.as_deref()
        } else {
            Some(app_id)
        };
        let (node_id, fd, new_token, pos) = select_stream(SourceType::Window, token).await?;
        // 若拿到了新的 token（首次选择），记录下来供下次复用。
        if let Some(t) = &new_token {
            eprintln!(
                "新的 restore_token = {}（请用它构造 ScreenCastCapturer 以复用）",
                t
            );
        }
        let img = extract_one_frame(node_id, fd)?;
        Ok((img, pos, new_token))
    }

    /// 与 `capture_app` 类似，但额外把本次（可能新生成的）restore_token 一并返回，
    /// 方便调用方持久化到配置，实现「提前赋权」。
    pub async fn capture_app_token(&self, app_id: &str) -> Result<(RgbaImage, Option<String>)> {
        let token = if app_id.is_empty() {
            self.token_window.as_deref()
        } else {
            Some(app_id)
        };
        let (node_id, fd, new_token, _pos) = select_stream(SourceType::Window, token).await?;
        let img = extract_one_frame(node_id, fd)?;
        Ok((img, new_token))
    }
}
