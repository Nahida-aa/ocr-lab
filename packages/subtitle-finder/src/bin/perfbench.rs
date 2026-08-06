//! 状态机关键函数微基准：隔离测 `compare_two_subs_optimal` / imgops 原语的真实开销。
//!
//! 目的：在整视频 pipeline 之外，单独测某个函数的每调用耗时，判断「分配优化」
//! 是否真的有墙钟收益。跑法：`cargo run -p subtitle-finder --release --bin perfbench`。
//!
//! 数据用 1280×720 合成字幕帧（几行白色文字），真实像素量级，函数语义与真帧一致。

use std::time::Instant;

use subtitle_finder::compare::compare_two_subs_optimal;
use subtitle_finder::imgops;
use subtitle_finder::params::Params;

const W: usize = 1280;
const H: usize = 720;

/// 合成一帧：底部有 1-2 行白字（255），其余 0。模拟 ISA 前景图。
fn make_text_frame(line1: bool, line2: bool) -> Vec<u8> {
    let mut im = vec![0u8; W * H];
    if line1 {
        for y in 20..40 {
            for x in 200..1080 {
                im[y * W + x] = 255;
            }
        }
    }
    if line2 {
        for y in 45..60 {
            for x in 150..1100 {
                im[y * W + x] = 255;
            }
        }
    }
    im
}

fn bench<T>(name: &str, iters: usize, mut f: impl FnMut() -> T) {
    // 预热。
    for _ in 0..10 {
        std::hint::black_box(f());
    }
    let start = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(f());
    }
    let dur = start.elapsed();
    let per = dur.as_secs_f64() / iters as f64;
    println!(
        "{:<40} {:>6} 次  {:>8.3} ms/次   ({:>8.2} ns/次, 总 {:>7.3}s)",
        name,
        iters,
        per * 1e3,
        per * 1e9,
        dur.as_secs_f64()
    );
}

fn main() {
    let p = Params::default();

    // 两帧：行1一致、行2有差异 → compare 会走完整路径（首轮或 DifficultCompare）。
    let im_a = make_text_frame(true, true);
    let im_b = make_text_frame(true, false);
    let ve_a = imgops::dilate(&im_a, W, H, 1);
    let ve_b = imgops::dilate(&im_b, W, H, 1);
    // 相同帧 → 返回 true（无变化），走 compare_two_subs 首轮直接命中。
    let im_same = make_text_frame(true, true);
    let ve_same = imgops::dilate(&im_same, W, H, 1);

    println!("=== compare_two_subs_optimal（1280x720）===");

    // 1) 相同帧：最热路径（追踪阶段 bln==true 时每帧都调）。
    let (a, b, va, vb) = (&im_a, &im_b, &ve_a, &ve_b);
    bench("compare_optimal 相同帧(无变化)", 2000, || {
        compare_two_subs_optimal(a, None, va, None, a, None, va, W, H, 0, W as i32 - 1, &p)
    });
    // 2) 差异帧：可能触发 DifficultCompare 的昂贵路径。
    bench("compare_optimal 差异帧", 2000, || {
        compare_two_subs_optimal(a, None, va, None, b, None, vb, W, H, 0, W as i32 - 1, &p)
    });

    println!("\n=== imgops 原语（单次整帧分配）===");
    bench("add_two_images", 5000, || {
        imgops::add_two_images(a, b, W * H)
    });
    bench("dilate(iters=1)", 2000, || {
        imgops::dilate(a, W, H, 1)
    });
    bench("intersect_two_images_inplace", 5000, || {
        let mut x = a.clone();
        imgops::intersect_two_images_inplace(&mut x, b, 0u8);
        x
    });

    // 参照：compare 相对 20ms/帧 变换的占比。
    println!(
        "\n参照：整视频每帧约 20ms（其中 filter 9ms + im_ff 7ms）。上面的 compare 每次 {}",
        "若远小于 1ms 则状态机分配非瓶颈"
    );
    let _ = im_same;
    let _ = ve_same;
}
