# 开发约定

## 开发记录文档必须持续更新

本仓库存在 `开发记录文档.md`(开发记录),描述项目架构、编译运行、协议、已知坑与当前环境 。

**规则:每次完成功能改动、修复、架构调整或依赖变更后,必须同步更新 `开发记录文档.md` 的 相关章节**,保持其与代码现状一致。

重点需同步的章节:

- 「三、编译与运行」:依赖、命令变化
- 「四、协议」:WsServer/WsClient 消息变更 (前后端同步)
- 「五、已知坑」:新发现的问题或已修复的坑
- 「六、当前运行环境」:运行方式、测试辅助变化

## 其他约定

- Rust 格式化:`cargo +nightly fmt --check`(CI 用 nightly rustfmt)
- 前端检查:`npm run check` / `npm run lint` / `npm run build`
- 服务端编译:`cargo build --workspace`
- 修改协议时两端 (`protocol.ts` / `protocol.rs`)必须同步
