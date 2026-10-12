# 两种发布方式的规范审计与验收计划

基准：`cs-ippeve11-20200515-5100.14.md`（PWG 5100.14-2020）。本文记录实现证据与待办，不将“属性存在”或“本机测试通过”等同于 IPP Everywhere 认证或 Apple 实机打印成功。

## 两个分支的边界

| 分支 | 对外身份 | 作业路由 | 必须单独验证 |
| --- | --- | --- | --- |
| `direct-printer-broadcast` | 每个 Windows 队列各有 DNS-SD 实例、UUID 和 `/ipp/print/<id>` | 按 URI 对应的实体队列投递；多队列并存 | 两队列同时广播、并发提交、撤销其中一台后另一台保持可用，默认 `/ipp/print` 别名仅在单队列时有效 |
| `virtual-gateway-printer` | 固定 AirPrinter 身份、UUID 和 `/ipp/print` | Create-Job / Print-Job 时锁定当前实体目标；切换后旧作业继续走原目标 | 切换时身份不变、旧作业不串队列、离线撤回、旧实体 URI 只匹配当前目标 |

`ipp/server.rs` 和 `mdns_broadcaster.rs` 在两分支的现有提交中相同。共同规范修复已分别进入两分支并各跑回归，不能只凭同源代码推断行为一致。

## MUST / SHOULD 审计项

| 规范条款 | 当前证据与问题 | 验收条件 |
| --- | --- | --- |
| 4.2.1–4.2.2 DNS-SD | 已移除实例名默认哈希，并在本进程中拒绝同名覆盖；仍缺 `_print._sub._ipp._tcp` 和 IPPS 对应子类型，同名碰撞后自动改名未实现 | 同一基础实例同时可由 Apple 与规范子类型发现；PTR 指向同一 SRV/TXT，停止共享后两种子类型均撤回；只有冲突时追加区分后缀 |
| 4.2.3 LOC | `mdns-sd` 当前接口未发布 LOC，也没有用户配置位置 | 提供位置配置并在每个相关实例发布可解析 LOC；未知精度按规范编码；不能实现前保留为明确偏差 |
| 4.2.4 TXT | `rp`、`pdl`、`TLS`、TXT 长度已有测试 | 抓取实际 mDNS 数据包，确认 `rp` 在前 400 字节、总长限制、IPP/IPPS 端口与 `printer-uri-supported` 一致 |
| 5.1.1–5.1.3 HTTP | 已统一校验唯一 Host；图标条件 GET 和 304、IPP POST `no-cache` 的本机回归通过 | 实机流量复核 Host、Last-Modified 和条件响应；新增公开资源继续走统一校验 |
| 5.2、表 4 操作 | 11 个操作码已声明并有部分协议测试；`Identify-Printer` 只记录日志，需核实其响应语义 | 每个声明操作均有正反例、真实 IPP 序列化回放，并与作业状态和 Windows 取消行为一致 |
| 5.3–5.8、表 5/7/8/10/11/12 | 现有测试检查属性出现，未逐项核对值、条件适用性及真正执行 | 建立表格逐项测试：值类型、单位、默认、支持范围、条件、不支持选项的错误；声明内容与驱动及后台实际行为一致 |
| 9.3 PDF 条件要求 | 非空 `document-password` 和任意 `overrides` 现明确拒绝；已撤销虚假的 `overrides-supported`。PDF 条件必需的 overrides 功能仍缺 | 实现页级覆盖的实际 Windows 输出及协议正反例，补回与实现一致的能力声明 |
| 5.3/5.4 设备事实 | 未测得的速度和边距使用 `unknown` (0x12)；IPP 型号与 DNS-SD `ty`/`product` 均为真实软件桥接器身份 | 从实际驱动获取能力时再精化；值及单位必须与打印执行保持一致 |
| 7.1 逻辑设备标志 | 两种方案均已同时报告 `ipp-everywhere` 和 `ipp-everywhere-server` | 保持逻辑设备标志一致；其余硬性项未满足前不宣称完整一致性 |
| 8、10、11 字符串/URI/安全 | Unicode 控制字符过滤、URI 来源和全部 HTTP Host 路径尚未形成覆盖 | 用含控制字符及多语言队列名、恶意 Host、IPv4/Bonjour/备用端口测试回放；不产生非法或不可达 URI |

## 实施顺序与发布门槛

1. **共同协议层**：修复 Host/条件 GET，审查业务属性的真实语义，再处理 DNS-SD 双子类型和位置记录。每项变更附条款、代码位置与失败前/通过后的测试。
2. **两种路由分别验收**：先直接广播两队列，再固定虚拟入口切换目标。回归应覆盖 Print-Job、Validate-Job、Create/Send/Close、查询、取消、停止共享和恢复。
3. **本机集成**：`cargo test --locked`、`cargo check --all-targets --locked`、`pnpm build`；网络抓包核对 PTR/SRV/TXT/LOC、HTTP 头及 IPP 编码；只用测试虚拟队列进行自动输出。
4. **真实设备验收**：从另一台 Apple 设备发现、提交 PDF/JPEG、多页及打印选项，观察收到文件、Windows spooler 和实体出纸三个独立阶段。未取得这些证据前，只发布开发构建和已验证范围，不标为 AirPrint/IPP Everywhere 完整通过。

两分支的 README、项目文档和测试记录已分别更新；直接广播分支描述逐队列发布，虚拟入口分支描述固定入口。仍需以真实 Apple 设备和实体打印机完成最终验收。
