# 声迹 SoundTrace

**本地会议录音库 —— 导入、离线转写、点字跳音频、全文检索、AI 会议纪要。**

会议录音不该躺在手机里吃存储，复盘也不该靠拖进度条反复听。声迹把录音集中到电脑统一管理：本地离线转写（内容不出本机）、转写稿与音频时间对齐（点字即跳）、全文检索秒级定位、一键生成 AI 会议纪要。

## 核心特性（V1）

- **导入归档**：拖拽 / 选文件夹批量导入，按内容哈希去重，按年月自动归档，支持 m4a / mp3 / wav / amr / 3gp 等手机常见格式（不兼容格式自动转码 m4a 副本）
- **离线转写**：FunASR 系 Paraformer-large 模型（ONNX，本地推理），中文优先优化，字符级时间戳；silero-VAD 分段 + CT-Transformer 自动标点
- **点字跳音频**：转写稿与波形播放器时间对齐，点击任意文字即跳转对应音频位置；跟随播放自动滚动
- **全文检索**：录音标题 / 标签 / 转写全文子串搜索（符合中文检索习惯）
- **AI 复盘**：转写稿一键生成会议纪要、行动项、关键决议（OpenAI 兼容 API，GLM / DeepSeek 等均可配置）
- **导出**：Markdown 会议纪要 / SRT 字幕 / 纯文本

## 技术栈

| 层 | 选型 |
|---|---|
| 壳 | Tauri 2（Rust，Windows 优先） |
| 界面 | React 19 + TypeScript + Vite + Tailwind CSS 4，深色主题，中文界面 |
| 数据 | SQLite（rusqlite bundled），库文件按年月目录归档 |
| 转写 | sherpa-onnx（Rust crate，静态链接）+ Paraformer-large int8 + silero-VAD + CT-Transformer 标点 |
| 复盘 | OpenAI 兼容 Chat Completions API（可配置 Base URL / Key / Model） |
| 媒体 | ffmpeg（转码、峰值预计算），波形前端自绘（Canvas + 预计算峰值，不解码大文件） |

数据目录：`~/SoundTrace/`（录音库、数据库、模型、缓存，可在设置中修改）。

## 开发

```bash
# 前置：Rust(MSVC) + Node 22 + pnpm + WebView2；ffmpeg 在 PATH（转写/峰值需要）
pnpm install
pnpm tauri dev      # 开发
pnpm tauri build    # 打包

# 图标：编辑 brand/icon.svg 后重新生成
cd tools/icon-gen && cargo run          # 渲染 1024 母图
cd ../.. && pnpm tauri icon brand/icon-1024.png
```

## 文档

- [docs/立项.md](docs/立项.md) —— 定位、边界、路线图（项目章程）

## 路线图

- [x] M0 立项 + 应用骨架
- [ ] M1 导入与库管理
- [ ] M2 波形播放器
- [ ] M3 离线转写 + 点字跳音频
- [ ] M4 全文检索 + AI 复盘 + 导出
- [ ] V2 说话人分离（3D-Speaker CAM++）、GPU 转写加速、收件夹监视

---
© 2026 Witer330 · 声迹 SoundTrace · 个人本地工具，录音与转写数据不出本机
