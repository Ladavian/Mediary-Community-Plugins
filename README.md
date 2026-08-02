# Mediary Community Plugins

面向 Mediary 的社区插件源码集合。每个插件位于 `plugins/<plugin-id>/`，拥有独立的 Rust
工程、Mediary 清单和发布元数据；仓库统一负责构建、Release 与商店上架自动化。

## 现有插件

| 插件 | 目录 | 说明 |
| --- | --- | --- |
| DC 助手 | `plugins/dockercopilot-helper/` | Docker Copilot 的更新检查、自动更新、镜像清理和备份 |
| 豆瓣将映 | `plugins/douban-coming/` | 按豆瓣想看人数筛选将映电视剧，提前订阅并发送开播提醒 |

## 发布一个插件

1. 在插件目录更新 `plugin.json` 的版本，并确保 `plugin-source.json` 中的二进制名、作者、权限和兼容版本正确。
2. 推送标签：`<plugin-id>-v<version>`，例如 `dockercopilot-helper-v1.0.4`。
3. GitHub Actions 自动运行测试、构建 Linux AMD64 安装包、生成 SHA-256 并创建不可变 Release。
4. 如已配置 `MEDIARY_STORE_TOKEN`，同一工作流会更新 `Ladavian/Mediary-Plugins` Fork，并向官方商店自动创建草稿 PR。

## 商店自动上架

在本仓库 Settings → Secrets and variables → Actions 中添加 `MEDIARY_STORE_TOKEN`。它应为专用
fine-grained PAT，只授权以下公开仓库的 Contents 读写和 Pull requests 读写：

- `Ladavian/Mediary-Plugins`（更新 Fork 分支）；
- `KyleYu2024/Mediary-Plugins`（创建上游 PR）。

未设置该 Secret 时，发布工作流仍会完成构建和 Release，但会跳过商店 PR，不会泄露或使用个人登录令牌。
