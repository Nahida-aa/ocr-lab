//! 逐帧解码（视频 → 帧流），复刻 VideoSubFinder 的 30fps 逐帧。
//!
//! 用 `ffmpeg-next` 解码视频，逐帧产出 BGR `Array3<u8>`（H×W×3），供
//! 预处理（交集/投影）使用。逐帧产出（对齐 VideoSubFinder 的 30fps 全量
//! 处理，避免抽样帧的时间偏移）。
//!
//! 提供两种用法：
//! - [`FrameStepper`]：**流式逐帧**（`next()` 按需拉帧，可暂停/续解），供
//!   `FrameCache` 做滑动窗口内存优化（对齐 C++ `RunSearch` 的环形缓冲）。
//! - [`for_each_frame`]：一次性全量回调解码（基于 `FrameStepper`，保持兼容）。

use anyhow::{Context, Result};
use ffmpeg_next::media::Type;
use ffmpeg_next::software::scaling::{context::Context as ScalerContext, flag::Flags};
use ffmpeg_next::util::frame::video::Video;
use ffmpeg_next::{Packet, format::context::Input};
use ndarray::Array3;

/// 流式逐帧解码器：持有 demuxer + 解码器 + 颜色转换器，`next()` 按需拉一帧。
///
/// `PacketIter` 是无状态包装（内部只调 `av_read_frame`），demux 位置存在 `Input`
/// 里，因此可暂停（丢弃不再调 `next`）后续解（再次调 `next` 从上次位置继续）。
/// 无需 seek / 重开视频。这让调用方只保留最近 N 帧的滑动窗口，避免全量驻留内存。
pub struct FrameStepper {
    ictx: Input,
    decoder: ffmpeg_next::codec::decoder::Video,
    scaler: ScalerContext,
    video_stream_index: usize,
    next_packet: Option<Packet>, // 预取的一包，跨越续接边界
    sent_eof: bool,
}

impl FrameStepper {
    /// 打开视频，准备流式解码。
    pub fn open(video: &std::path::Path) -> Result<Self> {
        ffmpeg_next::init().context("ffmpeg 初始化失败")?;

        let ictx = ffmpeg_next::format::input(video).context("打开视频失败")?;
        let input = ictx
            .streams()
            .best(Type::Video)
            .ok_or_else(|| anyhow::anyhow!("视频没有视频流"))?;
        let video_stream_index = input.index();

        let context_decoder =
            ffmpeg_next::codec::context::Context::from_parameters(input.parameters())
                .context("创建解码上下文失败")?;
        let decoder = context_decoder.decoder().video().context("创建视频解码器失败")?;

        let scaler = ScalerContext::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg_next::format::Pixel::BGR24,
            decoder.width(),
            decoder.height(),
            Flags::BILINEAR,
        )
        .context("创建颜色/尺寸转换失败")?;

        Ok(Self {
            ictx,
            decoder,
            scaler,
            video_stream_index,
            next_packet: None,
            sent_eof: false,
        })
    }

    /// 拉下一帧（BGR `Array3`），EOF 返回 `None`。
    pub fn next(&mut self) -> Result<Option<Array3<u8>>> {
        loop {
            // 若无预取包且未 EOF，从 demuxer 取下一视频包。
            // packets() 迭代器借用 ictx，仅在块内使用（取完即 drop），
            // 下次 next() 重新获取迭代器会从当前 demux 位置继续（调查确认）。
            if self.next_packet.is_none() && !self.sent_eof {
                let mut it = self.ictx.packets();
                while let Some((stream, packet)) = it.next() {
                    if stream.index() == self.video_stream_index {
                        self.next_packet = Some(packet);
                        break;
                    }
                }
                drop(it);
                if self.next_packet.is_none() {
                    // 视频包耗尽 → send_eof 触发 drain。
                    self.decoder.send_eof().context("send_eof 失败")?;
                    self.sent_eof = true;
                }
            }

            // 送一个包给解码器（若还有）。
            if let Some(pkt) = self.next_packet.take() {
                self.decoder.send_packet(&pkt).context("send_packet 失败")?;
            }

            // 尝试收一帧。
            let mut decoded = Video::empty();
            match self.decoder.receive_frame(&mut decoded) {
                Ok(_) => {
                    let mut bgr = Video::empty();
                    self.scaler.run(&decoded, &mut bgr).context("帧转换失败")?;
                    return Ok(Some(frame_to_array3(&bgr)));
                }
                Err(ffmpeg_next::Error::Eof) => {
                    // EOF：若还没发完包（可能下一个包还有帧），继续循环取包；
                    // 若已 send_eof 且没有更多帧，真结束。
                    if self.next_packet.is_none() && self.sent_eof {
                        return Ok(None);
                    }
                    // 否则继续循环取下一包。
                }
                Err(_) => {
                    // receive_frame 需要更多数据：继续循环送下一 packet。
                    continue;
                }
            }
        }
    }
}

/// 一次性全量回调解码（基于 `FrameStepper`）。`f` 返回 `false` 可提前停止。
///
/// `f` 收到一帧 BGR `Array3<u8>`（H×W×3，0-255）。
pub fn for_each_frame(
    video: &std::path::Path,
    mut f: impl FnMut(Array3<u8>) -> Result<bool>,
) -> Result<()> {
    let mut stepper = FrameStepper::open(video)?;
    while let Some(arr) = stepper.next()? {
        if !f(arr)? {
            break;
        }
    }
    Ok(())
}

/// 把 BGR24 参考帧（`frame.data(0)` 行优先，stride 可能带 padding）转成连续 `Array3`。
fn frame_to_array3(frame: &Video) -> Array3<u8> {
    let h = frame.height() as usize;
    let w = frame.width() as usize;
    let stride = frame.stride(0) as usize;
    let data = frame.data(0);
    let mut out = Array3::<u8>::zeros((h, w, 3));
    for y in 0..h {
        for x in 0..w {
            out[[y, x, 0]] = data[y * stride + x * 3 + 0];
            out[[y, x, 1]] = data[y * stride + x * 3 + 1];
            out[[y, x, 2]] = data[y * stride + x * 3 + 2];
        }
    }
    out
}
