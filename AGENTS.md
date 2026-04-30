# AGENTS.md — facto-cli

TypeScript CLI 工具（x402 支付、服务发现、wallet 管理）。Bun runtime。

## 完整项目上下文

见 `../facto-engine/docs/harness/AGENTS.md`（项目级锚点 repo）。

## 本 repo 局部

- `src/`          CLI 命令源码
- `bin/`          入口脚本
- `package.json`  依赖

## 本 repo 验证

| Level | 命令 |
|---|---|
| L0 | `bun run lint` |
| L1 | `bun run typecheck` |
| L2 | `bun test` |
| L4 | `bun run build` |

## 跨 repo 集成点

- 后端 API 契约：`../facto-engine/docs/harness/contracts/http-api.md`
- 跨 repo 改动通知：`../facto-engine/docs/harness/cross_repo_changes.md`
