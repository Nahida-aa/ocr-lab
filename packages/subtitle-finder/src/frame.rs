//! 逐帧解码（视频 → 帧流），复刻 VideoSubFinder 的 30fps 逐帧。
//!
//! 用 `ffmpeg-next` 解码视频，逐帧产出 BGR `Array3<u8>`（H×W×3），供
//! 预处理（交集/投影）使用。逐帧产出（对齐 VideoSubFinder 的 30fps 全量
//! 处理，避免抽样帧的时间偏移）。
//!
//! 用回调模式（`for_each_frame`）而非迭代器，规避 ffmpeg-next 的
//! `PacketIter` 借用 `Input` 导致的自引用问题。

use anyhow::{Context, Result};
use ffmpeg_next::format::Pixel;
use ffmpeg_next::media::Type;
use ffmpeg_next::software::scaling::{context::Context as ScalerContext, flag::Flags};
use ffmpeg_next::util::frame::video::Video;
use ndarray::Array3;

/// 逐帧解码视频，每帧调用 `f`。`f` 返回 `false` 可提前停止。
///
/// `f` 收到一帧 BGR `Array3<u8>`（H×W×3，0-255）。
pub fn for_each_frame(
    video: &std::path::Path,
    mut f: impl FnMut(Array3<u8>) -> Result<bool>,
) -> Result<()> {
    ffmpeg_next::init().context("ffmpeg 初始化失败")?;

    let mut ictx = ffmpeg_next::format::input(&video).context("打开视频失败")?;
    let input = ictx
        .streams()
        .best(Type::Video)
        .ok_or_else(|| anyhow::anyhow!("视频没有视频流"))?;
    let video_stream_index = input.index();

    let context_decoder =
        ffmpeg_next::codec::context::Context::from_parameters(input.parameters())
            .context("创建解码上下文失败")?;
    let mut decoder = context_decoder.decoder().video().context("创建视频解码器失败")?;

    let mut scaler = ScalerContext::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        Pixel::BGR24,
        decoder.width(),
        decoder.height(),
        Flags::BILINEAR,
    )
    .context("创建颜色/尺寸转换失败")?;

    let mut packet_iter = ictx.packets();
    let mut eof_sent = false;
    loop {
        // 送 packet 或 EOF。
        if !eof_sent {
            if let Some((stream, packet)) = packet_iter.next() {
                if stream.index() == video_stream_index {
                    decoder.send_packet(&packet).context("send_packet 失败")?;
                }
            } else {
                decoder.send_eof().context("send_eof 失败")?;
                eof_sent = true;
            }
        }

        // 尝试收一帧。
        let mut decoded = Video::empty();
        match decoder.receive_frame(&mut decoded) {
            Ok(_) => {
                let mut bgr = Video::empty();
                scaler.run(&decoded, &mut bgr).context("帧转换失败")?;
                let arr = frame_to_array3(&bgr);
                let keep_going = f(arr)?;
                if !keep_going {
                    return Ok(());
                }
            }
            Err(ffmpeg_next::Error::Eof) => return Ok(()),
            Err(_) => {
                // receive_frame 需要更多数据：继续循环送下一个 packet。
                continue;
            }
        }
    }
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
