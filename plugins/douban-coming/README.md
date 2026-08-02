# 豆瓣将映

Mediary Rust 版“豆瓣将映”。它从 RSSHub 的豆瓣将映电视剧路由读取条目，按想看人数筛选，
解析为 TMDB 剧集后在开播窗口内创建 Mediary 订阅，并可在开播前发送一次提醒。
安装时会自动授予读取目录、读取/创建订阅和发送通知所需的最小权限。

原始功能思路来自 [luanyi143/MoviePilot-Plugins](https://github.com/luanyi143/MoviePilot-Plugins/tree/main/plugins.v2/doubancomingnotice)，原项目采用 GPL-3.0；本实现保留来源说明并面向 Mediary 的公开插件接口重写。

## 行为

- 仅处理经 Mediary TMDB 解析后确认是剧集的结果；
- 已存在的 Mediary 订阅不会重复创建；
- 开播日期优先使用 TMDB 解析结果，缺失时回退 RSS 描述中的日期；
- 提醒通过 `notifications:send` 发送，并在插件数据中去重；
- “清理历史”只会删除插件自身记录。
