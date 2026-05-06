# 赛道二子文档：弹幕游戏 `examples/gui_danmaku.bfs`

## 作品定位

创意赛道作品为：

- [`examples/gui_danmaku.bfs`](../examples/gui_danmaku.bfs)

这是一份用 BFS 编写的图形界面弹幕游戏。BFS 脚本通过 `setpixel` 与键盘输入接口
驱动 `bf-gui` 提供的 256x256 GUI 画面。

## 构建

先构建 BFS 编译器和 GUI：

```bash
cargo build --release --features gui --bin bf-gui --bin bfsc
```

## 运行方式

先将 `.bfs` 编译为 `.bf`：

```bash
./target/release/bfsc examples/gui_danmaku.bfs -o /tmp/gui_danmaku.bf
```

再启动 GUI：

```bash
./target/release/bf-gui /tmp/gui_danmaku.bf
```

也可以直接读取 `.bfs`：

```bash
./target/release/bf-gui examples/gui_danmaku.bfs
```

## 基本玩法

- 玩家位于屏幕底部
- 需要在六个波次中躲避不同类型的弹幕
- 存活至全部波次结束即获胜

## 操作

- `a` / `d`：左右移动
- `w` / `Space`：朝最近一次水平方向短距离冲刺
- `q` / `Escape`：退出

## 技术说明

- GUI 窗口由 `tauri` 提供
- Rust 侧维护 256x256 RGB332 帧缓冲
- BFS 脚本通过保留命令 `0xFE x y color` 写像素
- 前端 `gui-assets/renderer.js` 订阅脏矩形帧并在画布中渲染
- 浏览器侧定时注入保留键值 `0` 作为世界时钟 tick

## 作品结构

- `examples/gui_danmaku.bfs`：游戏逻辑
- `src/gui.rs`：GUI 启动、IPC、解释器线程管理
- `src/runtime/gui_io.rs`：像素输出、键盘输入与帧发布
- `gui-assets/`：前端渲染资源

## 运行注意事项

- Linux 下需要可用图形桌面环境
- `bf-gui` 在 Linux 下会自动以兼容方式重启自身，以规避部分 WebKit/Wayland 兼容问题
- GUI 作品不参与传统赛道自动评测，但可直接运行展示
