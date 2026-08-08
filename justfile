# 仓库根入口：跨包 / 跨目录的杂项脚本。
# 用法（在仓库根下）：
#   just                          # 默认 = 显示任务列表
#   just rename-strip <dir>       # 去掉目录下文件名的前缀（默认 frame_），对接 subtitle-ocr 的 --dir 时间命名
#   just rename-strip <dir> --prefix shot_   # 去掉其他前缀（如 shot_）
#   just rename-strip <dir> --dry-run         # 只预览、不改名
#
# 说明：subtitle-ocr 的 --dir 要求文件名本身即时间数值（ms / ms_ms），不允许 frame_ 等
# 语义前缀，故抽帧产出的 frame_0000030.jpg 需先改名为 0000030.jpg 才能被识别。

# ---- 默认任务（`just` 无参即显示列表） ----
default:
    @just --list

# ---- 批量去掉文件名前缀（默认 frame_），对接 subtitle-ocr --dir 命名约定 ----
# 用法：just rename-strip <dir> [extra-args...]
#   extra-args 透传给脚本：--prefix <p> / --dry-run
rename-strip dir extra_args="":
    node scripts/rename_strip_prefix.mjs {{dir}} {{extra_args}}
