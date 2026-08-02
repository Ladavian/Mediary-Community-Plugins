# DC 助手（Mediary Rust 版）

这是将 `gxterry/MoviePilot-Plugins` 的 DC 助手迁移为 Mediary 插件 API v1 的 Rust 实现。
它连接 Docker Copilot API，支持更新检查、指定容器自动更新、更新进度汇总、无标签未使用镜像清理，以及容器配置备份。

原插件作者为 gxterry；本项目为面向 Mediary 的独立 Rust 重新实现，并保留来源说明。

## 与原插件的差异

Mediary 不提供 MoviePilot 的内部消息推送接口。因此每次计划动作会返回标准 `notice` 和详细 `report`，由 Mediary 展示结果；最近一次成功执行也会保存到插件的数据视图。插件不会访问 Mediary 内部数据库或宿主凭据。

原插件的镜像清理代码与说明不一致；本实现依说明处理：仅清理 `inUsed=false` 且没有有效标签（空值、`<none>` 或空标签列表）的镜像。

## 构建与打包

在具备 Rust 工具链的 Linux 环境执行：

```bash
./build.sh 1.0.5 x86_64-unknown-linux-gnu
```

会生成 `dockercopilot-helper-1.0.5-linux-amd64.tar.gz`。压缩包根目录直接包含 `plugin` 和 `plugin.json`，可按 Mediary 插件开发指南安装。GitHub Release 同时提供 Linux AMD64 与 ARM64 安装包。

## 配置

- **Docker Copilot 地址**：例如 `http://192.168.1.10:12712`。
- **Secret Key**：用于每次请求生成 HS256 JWT；该项作为密码字段处理，且不会写入日志或数据文件。
- **容器选择**：在插件的“容器选择”页面中从 Docker Copilot 自动加载的下拉多选框勾选；保存后，更新检查与自动更新任务会使用对应选择。检查更新留空表示检查全部；自动更新留空表示不更新任何容器。
- **Cron**：使用五段表达式，按 Mediary 宿主时区运行。

Docker Copilot Zspace 项目已归档。请确保所用服务仍兼容其 `/api/containers`、`/api/images`、`/api/container/{id}/update`、`/api/progress/{taskID}` 和 `/api/container/backup` 接口。
