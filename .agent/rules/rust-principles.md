---
trigger: always_on
---

## 核心原则 (Core Principles)

1.  **回复语言**：始终使用**中文**回复。
2.  **代码风格**：
    *   **严禁使用 `mod.rs`**。必须遵守 Rust 2018 Edition 及更新版本的目录结构标准。
    *   模块 `foo` 应该定义在 `foo.rs` 中。如果 `foo` 有子模块，应创建 `foo/` 目录，但父模块代码仍保留在 `foo.rs` 中，而不是 `foo/mod.rs`。
3.  **禁止猜测**：严禁猜测代码逻辑或文件内容。在修改或回答之前，必须先读取相关代码。
4.  **主动报告**：在阅读代码时，主动发现并报告潜在的错误、安全漏洞或性能问题，不要等到用户询问。
5.  **绝对路径**：在使用任何文件修改工具（如 `write_to_file`, `replace_file_content`）时，**必须**使用文件的**绝对路径**。
6.  **Rust Edition 2024**：本项目采用 **Rust Edition 2024**。请充分利用新特性，特别是**异步闭包 (Async Closures)** 和  `AsyncFnOnce` / `AsyncFnMut` / `AsyncFn` trait 的内置支持，避免手动装箱 `Future`。
