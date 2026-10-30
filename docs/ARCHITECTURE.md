# oxiron 架构设计

> 状态：**草稿 v0.1**（M0 阶段定稿用）
> 日期：2026-10-30
> 定位：对标 Spring Boot 的 Rust 全栈应用框架

---

## 1. 项目定位与对标原则

### 1.1 一句话定位

oxiron 是一个 Rust 全栈应用框架，目标是让 Rust 开发者获得与 Spring Boot 同级的**开发体验**：约定优于配置、依赖自动装配、起步依赖开箱即用、生产就绪的可观测能力。

### 1.2 对标的三条原则

**对标的是开发体验，不是 API 照抄。** Spring Boot 的运行时魔法依赖 Java 的反射、GC 与字节码生成，Rust 一概没有。直接翻译 API 只能得到一个四不像。以下三条原则是所有设计决策的裁决依据：

| 原则 | 含义 | 反例（禁止） |
|---|---|---|
| **编译期优于运行时** | 装配、校验、代码生成尽量发生在编译期，运行时零开销 | 运行时字符串查找 bean、`Any` 向下转型 |
| **显式优于魔法** | 宏展开后是普通 Rust 代码，可调试、可单测；魔法只做语法糖 | 无法 trace 的隐藏行为、全局可变状态 |
| **Rust 独有红利是卖点** | SQL 编译期校验、feature 精细裁剪、零成本抽象、强类型错误 | 为了"像 Java"而弱化类型系统 |

### 1.3 非目标（Non-goals）

- ❌ 不做 Spring API 的兼容层（不叫 `@RestController`，不复刻包名）；
- ❌ 不支持同步运行时（只支持 tokio）；
- ❌ M1–M5 期间不做微服务治理全家桶（服务发现、配置中心、熔断限流留到 1.0 之后）；
- ❌ 不自研 HTTP 协议栈、连接池等基础设施（站在 axum/tower/sqlx 的肩膀上）；
- ❌ 不追求 `oxiron-core` 之外的零依赖（框架必然依赖生态，零依赖只保留给核心叶子 crate）。

---

## 2. Spring Boot 能力映射

| Spring Boot 支柱 | oxiron 对应 | 承载 crate | 备注 |
|---|---|---|---|
| IoC / DI 容器（@Autowired） | 编译期宏装配 + 手动容器 | `oxiron-core` + `oxiron-macros` | 见 §3.1 决策 |
| AOP / 拦截器 | `tower::Layer` 中间件链 | `oxiron-web` | 动态代理 → 函数组合 |
| 自动装配 + starter | feature + builder + starter 聚合 crate | `oxiron-starter-*` | 见 §3.3 |
| 内嵌服务器（Tomcat） | tokio + hyper/axum | `oxiron-web` | 不自研 |
| Spring MVC（@RestController） | 路由宏 `#[controller]` `#[get]` | `oxiron-macros` | 语法糖层 |
| Spring Data JPA | sqlx 封装 + repository 宏 | `oxiron-data` | SQL 编译期校验是差异点 |
| application.yml + profiles | toml + env 覆盖 + profile | `oxiron-config` | `application-dev.toml` |
| @ConfigurationProperties | 类型化配置结构体 + derive | `oxiron-config` | |
| Spring Security | 鉴权中间件 + extractor | `oxiron-security`（M4） | |
| Bean Validation | `validator` + derive 宏 | `oxiron-web` | |
| Actuator | `/actuator/health` `/actuator/metrics` | `oxiron-actuator` | |
| MockMvc | `tower::ServiceExt::oneshot` | 测试工具模块 | |
| start.spring.io | `oxiron new` CLI 脚手架 | `oxiron-cli` | M5 |

---

## 3. 核心设计决策（ADR 摘要）

### 3.1 D1：DI 容器 —— "机制与语法糖分离"的编译期装配 ★最关键

**候选方案对比：**

| 方案 | 代表 | 优点 | 缺点 | 结论 |
|---|---|---|---|---|
| 运行时反射容器 | Spring | 灵活 | Rust 无反射，**不可行** | 排除 |
| 手动 builder 注册 | axum `State`、shaku | 简单、类型安全、易调试 | 无"自动装配"体验 | ✅ 作为机制层 |
| 全局注册表（`inventory`/`linkme`） | — | 接近 Spring 的按名查找 | 类型擦除、运行时查找、`Any` 下转型 | 仅作补充 |
| 编译期 proc-macro 装配 | spring-rs | 自动装配体验、零运行时开销 | 宏复杂、编译耗时、报错信息难做 | ✅ 作为语法糖层 |

**决策：两层架构。**

- **机制层（`oxiron-core`）**：一个显式的类型化容器（builder 注册 + `OnceCell` 单例 + 工厂函数原型作用域），不依赖任何宏。可单测、可 debug、可脱离框架使用。保持零依赖或仅依赖 `tracing`。
- **语法糖层（`oxiron-macros`）**：`#[component]`、`#[autowired]`、`#[controller]` 等 proc-macro，**只负责生成对机制层的普通 Rust 调用**。宏尽量薄，逻辑全在 core。

这样宏一旦出问题，用户可以退回手写注册代码，框架不会被宏绑架；宏展开结果等价于用户手写代码，满足"显式优于魔法"。

**Bean 作用域映射：**

| Spring | oxiron | 实现 |
|---|---|---|
| Singleton | `singleton` | `OnceCell` / `LazyLock` |
| Prototype | `factory` | 每次调用工厂函数新建 |
| Request | `request`（M4） | 请求上下文携带 |

### 3.2 D2：Web 底座 —— 封装 axum/tower，不自研

axum 的 `Handler` + `Router` + `tower::Layer` 体系与我们的中间件/AOP 模型天然契合。`oxiron-web` 提供的是**上层约定**（路由宏、统一异常处理、配置化端口），axum 类型通过 `prelude` 适度重导出，用户需要时可直接下潜到 axum。

### 3.3 D3：自动装配与 starter —— Rust 化的"约定优于配置"

- **starter 聚合 crate**（如 `oxiron-starter-web`）：本身几乎无代码，只做依赖聚合 + 一份**默认自动配置**（内置 `application.toml` 默认值）；
- **自动配置 = 合理默认值 + 显式覆盖**：`Oxiron::web().run()` 开箱即用，任何默认值都可被配置文件或 builder 覆盖；
- cargo feature 负责能力裁剪（如 `actuator`、`metrics`），不装的代码不进二进制。

### 3.4 D4：错误处理 —— `thiserror` 体系

框架公开 API 一律 `Result<T, oxiron::Error>`（`thiserror` 定义的非穷举错误枚举 `#[non_exhaustive]`），**库代码绝不 `unwrap()`**。web 层提供到 HTTP 状态码的默认映射 + 可自定义的全局异常处理器（对标 `@ControllerAdvice`）。

### 3.5 D5：异步与工具链

- 异步运行时：**tokio**（唯一现实选择）；
- MSRV：**1.85**，Edition 2024（与当前 Cargo.toml 一致）；
- 质量门禁：`cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`、`cargo doc --no-deps`，全部进 CI。

---

## 4. 模块划分与依赖规则

```
oxiron/  （cargo workspace）
├── Cargo.toml                  # [workspace] 统一依赖版本、lint 配置
├── crates/
│   ├── oxiron-core             # 容器、生命周期、扩展点        [零依赖]
│   ├── oxiron-macros           # proc-macro 语法糖             [syn/quote]
│   ├── oxiron-config           # 配置、profiles、类型化配置     [figment|config-rs]
│   ├── oxiron-web              # 路由、中间件、异常处理         [axum, tower, tokio]
│   ├── oxiron-data             # 连接池、事务、repository 宏    [sqlx]
│   ├── oxiron-security         # 认证授权中间件                [M4]
│   ├── oxiron-actuator         # /actuator/* 生产就绪端点      [web]
│   └── oxiron-starter-web      # 起步依赖聚合 + 默认自动配置
├── examples/                   # 面向用户的示例程序
├── docs/                       # 架构文档、指南
└── oxiron-cli                  # oxiron new 脚手架 [M5]
```

**依赖规则（CI 用 `cargo-deny` 或手工评审把关）：**

1. `oxiron-core` 不依赖 workspace 内任何 crate，不依赖 web/async 生态（零依赖基因保留在这里）；
2. `oxiron-macros` 只依赖 `syn`/`quote`/`proc-macro2`，生成的代码只调用 `oxiron-core`；
3. 依赖方向单向：`starter-* → web/actuator/config → core`，禁止反向；
4. `oxiron-data` / `oxiron-security` 相互独立，只能依赖 core/config；
5. workspace 外的依赖版本统一在根 `Cargo.toml` 用 `[workspace.dependencies]` 管理。

---

## 5. 目标开发体验（M1 验收形态）

用户侧代码——**10 行跑起一个 REST 服务**：

```rust
use oxiron_starter_web::prelude::*;

#[get("/hello")]
async fn hello() -> &'static str {
    "Hello, oxiron!"
}

#[oxiron::main]
async fn main() {
    Oxiron::web().run().await;   // 配置外置 application.toml，自动装配
}
```

进阶形态——依赖注入与分层（M3 验收形态）：

```rust
#[component]
struct UserService {
    repo: UserRepository,          // #[autowired] 语义，编译期装配
}

#[controller]
struct UserController {
    service: UserService,
}

#[get("/users/{id}")]
async fn get_user(Path(id): Path<u64>, State(svc): State<UserService>) -> Result<Json<User>> {
    Ok(Json(svc.find(id).await?))
}
```

配置文件：

```toml
# application.toml
[server]
port = 8080

[profiles.dev]         # cargo run -- --profile dev 激活
log_level = "debug"
```

---

## 6. 里程碑与验收标准

| 里程碑 | 内容 | 验收标准（可验证） |
|---|---|---|
| **M0** | workspace 骨架 + 本文档定稿 | 多 crate `cargo test` 通过；D1–D5 决策评审通过 |
| **M1** | Web MVP | §5 第一段代码可运行，`GET /hello` 返回 200；`oneshot` 集成测试通过 |
| **M2** | 配置 + profiles + Actuator | 端口/日志级别可由 `application.toml` + env 覆盖；`GET /actuator/health` 返回 JSON 状态 |
| **M3** | data starter + 事务 | 一个 Repository 完成 CRUD；错误类型映射到 HTTP 状态码；`#[component]`/装配宏落地 |
| **M4** | 安全 + 校验 + 可观测 | 鉴权中间件链可配置；`tracing` 结构化日志；请求校验 derive |
| **M5** | CLI 脚手架 + 文档站 | `oxiron new my-app` 生成的项目 `cargo run` 即跑；docs.rs + 指南上线 |

**版本策略**：0.x 期间 API 允许破坏性变更（每次破坏在 CHANGELOG 标注），1.0 前完成 API 冻结评审。语义化版本严格执行。

---

## 7. 竞品与差异化

| 竞品 | 定位 | 与 oxiron 的关系 |
|---|---|---|
| [spring-rs](https://github.com/spring-rs/spring-rs) | 明确对标 Spring Boot 的 Rust 框架 | **最直接对手**，M0 期间必须深度调研；差异化：starter 体系、Actuator 生产就绪闭环、编译期 SQL、中文文档 |
| [loco.rs](https://loco.rs) | 对标 Rails 的全栈框架（脚手架+ORM+任务） | 全家桶路线的另一种答案，关注其脚手架设计 |
| axum / actix-web / poem / salvo | Web 层框架 | 潜在底座而非对手，oxiron 是其上的"约定层" |
| shaku | 独立 DI 容器 | D1 机制层的参考实现 |

**差异化主张**：现有对标品均未完整覆盖"starter 依赖聚合 + 自动装配 + Actuator 生产就绪"闭环；中文开发者社区的框架级文档几乎空白。

---

## 8. 风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| 编译期 DI 的生命周期/借用问题（`'static` 约束、循环依赖） | D1 可能返工 | **M0 先做 spike 原型**验证可行性，机制层/语法糖层分离保证可降级 |
| spring-rs 等竞品先行 | 方向撞车 | M0 深度调研，明确差异化清单后再动工 |
| proc-macro 拖慢编译、报错信息差 | 开发体验受损 | 宏保持薄，逻辑在 core；`trybuild` 测试宏报错信息 |
| 范围蔓延（对标品太大） | 项目烂尾 | Non-goals 约束 + 里程碑验收标准制，每个 M 只做验收清单内事项 |
| 单人/小团队维护 | 进度失控 | starter 化拆分让社区可增量参与；先做减法 |

---

## 9. 术语对照表

| Spring 生态 | oxiron | 说明 |
|---|---|---|
| Bean / `@Component` | Component / `#[component]` | 受容器管理的实例 |
| `@Autowired` | `#[autowired]` / 字段直接声明 | 编译期装配 |
| `@RestController` + `@RequestMapping` | `#[controller]` + `#[get]`/`#[post]` | 路由宏 |
| `@ControllerAdvice` | 全局异常处理器 | web 层 |
| spring-boot-starter-* | oxiron-starter-* | 依赖聚合 + 默认自动配置 |
| Actuator | oxiron-actuator | `/actuator/health` 等 |
| Profile (`application-dev.yml`) | profile (`application-dev.toml`) | 配置分环境 |
| start.spring.io | `oxiron new` | 项目脚手架 |
| MockMvc | `oneshot` 测试助手 | 集成测试 |

---

## 10. M0 立即行动项

- [ ] 深度调研 spring-rs：DI 实现、宏设计、配置体系，产出对比笔记；
- [ ] DI spike：用 `oxiron-core` 原型验证"builder 注册 + `OnceCell` 单例 + 宏生成装配代码"可行性；
- [ ] workspace 重构：`crates/` 多 crate 骨架，清理 `add` 占位代码与过时 README 描述；
- [ ] 本文档评审定稿，决策 D1–D5 签字确认。
