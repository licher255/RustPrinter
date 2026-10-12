# 后端审核与本地模拟记录

日期：2026-09-23。目标：将 PC 已安装驱动的实体队列发布到局域网，供 iPhone/iPad/Mac 通过 IPP/AirPrint 直接打印。

## 审核结果与修复

| 原问题 | 当前实现 |
| --- | --- |
| 接收后弹出 PrintDialog，失败时调用默认程序打开文件 | Windows.Data.Pdf / 图像或光栅解码 → GDI → 指定 Windows 队列；没有浏览器或打印对话框降级 |
| 全局只有一个打印机名称，所有广播共用 `/ipp/print` | 每队列独立、稳定且有长度限制的资源路径；路由和作业都绑定实际 Windows 队列 |
| mDNS 基础服务和子类型重复注册，SRV 主机名混用服务名 | 同一个服务实例关联 `_universal` PTR；SRV 指向正常 `.local.` 主机名；自动更新 IPv4 地址 |
| 每分钟注销再注册，停止共享不完整 | 由 mDNS daemon 维护记录；按队列注销并等待确认，最后关闭监听与 daemon |
| 广播不存在的 AirPrinter255、PDF/OneNote 等交互队列 | 移除虚构队列入口；正式列表过滤已知交互目的地 |
| 假称支持 URF/JPEG，所有数据都保存为 `.pdf` | 按 MIME 和签名校验；真正解码 URF、PWG Raster、JPEG、PDF |
| 尚未打印就返回 job-state=completed | 先 pending，再 processing；查询 Windows spooler，驱动失败时 aborted |
| 作业 ID 复用 request-id，取消和查询只是空响应 | 独立递增 ID，Create/Send/Get/Cancel 与队列绑定，取消投递到 Windows |
| 彩色/双面/份数等能力硬编码 | 查询 Windows 驱动能力；不支持的选项返回错误；纸张按驱动尺寸映射 |
| 中文名称按字节切片可能 panic | UTF-8 边界截断；队列原名保存，URI 使用 UUID v5 |
| 诊断脚本对合法 DNS RR 解包失败 | 重写边界检查、压缩指针、PTR/SRV/TXT/A/AAAA 解析，覆盖循环指针和截断包 |
| README 宣称完整 IPP Everywhere、建议关闭防火墙 | 明确实际实现范围；提供按 EXE、专用网络、本地子网放行的脚本 |

## 已获得的证据

- 后端测试二进制 `airprinter-6d9768ab92ef346f.exe --include-ignored --nocapture`：12 passed，0 failed，0 ignored。
- 默认单元/协议测试覆盖 UTF-8、队列状态、不同打印机的 URI/名称、撤销后 404、监听端口释放、自定义纸型、Create/Cancel/Validate、发送文档边界、驱动失败不报完成、光栅像素及截断数据。
- 本机 mDNS 真实网络测试：两个明确命名的模拟实例分别解析到对应资源/端口；停止 A 后收到 goodbye，B 保留；最终均清理。
- 四种文档格式都通过 Microsoft Print to PDF 静默输出。PDF 测试使用首次生成的 PDF 再渲染打印，验证 Windows PDF 渲染器实际被调用。
- 完整本地 HTTP 测试：发送 URF Print-Job，返回 pending，通过 Get-Job-Attributes 等待 Windows 队列完成，生成 `src-tauri/target/airprint-simulation/http-urf-13664.pdf`。
- 最终测试生成的 `http-urf-13664.pdf` 经 Poppler 检查为 A4、1 页、PDF 1.7；渲染后人工检查红、绿、蓝、白四个区域的位置和颜色正确。
- `python scripts/diagnose_mdns.py --self-test` 通过。
- `pnpm --config.verify-deps-before-run=false run build` 通过 TypeScript 与 Vite 构建。
- `cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol --locked --offline` 通过；生成内嵌前端的 `src-tauri/target/debug/airprinter.exe`。
- Windows 当前检测结果为 `[]`：只有 PDF/XPS/OneNote/Fax 等虚拟目的地，没有连接实体打印机，与用户提供的状态一致。

模拟输出是测试制品；正式网络请求不能指定输出文件，也不能启用测试后端。测试使用 Microsoft 虚拟队列，无实体打印。

## 尚未证明的最终条件

1. 另一台实际 iPhone/iPad/Mac 在系统打印面板中发现此 PC 的共享队列。当前 mDNS 测试是在本机运行，不能证明跨 Wi-Fi/网线、路由器或防火墙可达。
2. 实体打印机正确出纸，包括多页、双面、份数、纸型及断纸/离线/取消行为。Windows 队列完成或作业消失本身不证明纸张已输出。
3. 全部 Apple 系统版本和应用的互操作性，以及 IPP Everywhere 1.1 的完整一致性/认证。项目没有将这些声明为已完成。

## 连接硬件后的验收

- Windows 本地测试页正常后，在 AirPrinter 列表选择真实队列共享。
- 从另一台局域网电脑运行 `python scripts/diagnose_mdns.py`，核对 `_universal` PTR、SRV 地址/端口、TXT `rp`。
- 分别从 iPhone/iPad/Mac 系统打印面板发送 PDF、照片和多页文件，核对内容、页数、颜色、方向、份数、双面和纸型。
- 两台队列并行提交，确认不会串队列；停止一台后新作业被拒绝且另一个仍可用。
- 测试离线、缺纸、取消、重新连接、PC 地址变化。跨设备无法发现时先检查专用网络放行规则与路由器隔离，不关闭整个防火墙。

相关实现：`src-tauri/src/services/{mdns_broadcaster.rs,ipp/server.rs,print_job.rs,raster.rs,windows_print.ps1,windows_printers.ps1}`。

## 续测：记住共享选择与隔离 JS 客户端

- 程序将已选择的 Windows 队列名原子写入用户配置目录；后台每 30 秒检测一次队列。离线或暂时消失时撤回 IPP/mDNS，队列恢复后按已保存的选择重新发布；手动“停止共享”会删除记忆。
- Windows 短时间保留 TCP 631，导致立即重启时绑定失败。服务改为在 631 已被占用时选择 8631–8699 中的空闲端口，并把实际端口公布在 DNS-SD SRV 记录中。防火墙脚本覆盖这些端口。
- 更新后的本地后端测试：14 passed，0 failed。新增测试覆盖选择持久化、模拟离线与重连、驱动能力刷新以及 631 被占用时选择可访问的备用端口。
- `node scripts/simulate_airprint.mjs 172.22.48.1` 通过。Rust 模拟服务限定在主机专用虚拟网卡；独立 Node.js 进程验证真实 IPP 属性、拒绝无效打印份数及错误 Host。该虚拟交换机不会把多播回送给本机，因此 JS 的 DNS-SD 响应由隔离 UDP 回环模拟器提供；这项结果不证明真实 mDNS 跨设备可见。
- 仍没有实体打印机，也没有实际 Apple 设备的系统打印发现及出纸证据；最终目标保持待验收。

## 续查：多网卡地址和 Windows Public 网络

- 检查 `mdns-sd` v0.11.5 源码：`ServiceInfo::get_addrs_on_intf` 按本机接口掩码筛选同网段地址。真实 WLAN 上的 192.168.3.249/24 不会将 Hyper-V 的 172.22.48.1 发布为该接口的 A 记录。
- 本机 WLAN 当前为 Windows `Public`，且未发现 AirPrinter 专属防火墙规则；即使 mDNS 守护进程在本机宣布成功，也不能据此推断其他设备可发现。
- 放行脚本现覆盖 Private/Public，两种配置都仅允许指定 AirPrinter EXE 接收本地子网发来的 TCP 631（及 8631–8699 备用端口）和 UDP 5353。应用增加“允许局域网访问”按钮，只有用户点击并接受 Windows 提权提示才会应用规则。本轮仅验证脚本语法及构建，**没有修改本机防火墙**。
- 仍需实体打印机、Apple 设备以及跨设备的网络验证，不能将本机模拟视为最终完成。

## 规范属性复核

对照 `cs-ippeve11-20200515-5100.14.pdf` 第 29–31、36、52 页：

- 修正 `multiple-operation-timeout` 名称；增加与实际查询实现一致的 `which-jobs-supported` 和打印机状态消息。
- 彩色队列使用 `print-color-mode-default=auto`；Windows 后端按实际 `SupportsColor` 决定默认输出。
- DNS-SD 增加型号、文档类别、优先级和按驱动计算的份数标志，保留 `rp` 在前；长中文队列名仍保持 TXT 总长 ≤ 400 字节。
- 文档第 52 页明确说明 `printer-device-id` 已不再必需，因此没有虚构实体打印机厂商及型号。
- 当前实现仍不满足表 4、表 5 的所有 IPP Everywhere 条目，没有宣称完整认证，也未发布规范要求的 `_print` 子类型。Apple 的 `_universal` 发现路径和跨设备打印仍需实机验证。
- 变更后本地全量模拟测试：16 passed，0 failed。

## 续查：IPP URI 的默认端口

- `ipp` crate 的序列化已保证响应操作属性从 `attributes-charset`、`attributes-natural-language` 开始，排除了属性顺序这一怀疑。
- 按 RFC 3510 与 RFC 8010，IPP URI 可省略默认 631；RFC 8010 的直接请求示例仍以 `Host: printer.example.com:631` 传输。原校验把两者用同一规则处理，合法的无端口 `printer-uri` 会被误判为 404。
- 现按 URI 默认端口 631 校验 `printer-uri` 和 `job-uri`，HTTP `Host` 仍按连接的实际端口检查；备用端口 8631–8699 的 URI 必须显式包含端口。
- 覆盖能力查询、Create-Job、按 Job URI 查询，以及 Bonjour `.local` 名称。更新后本地模拟测试 17 passed，0 failed。跨 Apple 设备和实体出纸仍待验收。

## 自动打印与打印意图（2026-10-09）

- 用户反馈：手机可通过 HTTPS 诊断页触发测试页出纸，系统 AirPrint 打印仍失败。诊断页实际先向手机下载 JPEG，再由手机通过 IPP Print-Job 上传；这项反馈支持小 JPEG 上传及指定队列路径可用，不证明系统 AirPrint 的能力协商、PDF/URF 或大文件上传已通过。
- 仓库现有 `test.txt` 只有失败实例的 DNS-SD 属性，没有该次失败的请求或 Windows 驱动错误日志；故障根因仍未确认。
- 日常流程保持手机系统打印面板提交 → PC 自动接收/渲染 → 指定 Windows 队列。临时文件自动清理，不要求下载素材、打开查看器或 PC 再次确认。
- 修复以前直接拒绝 page-ranges 的缺口，接受正数、升序、不重叠的单段/多段范围；PDF 跳过未选择页的渲染，其他格式按收到的页序选择。如果范围内没有实际页面，不提交空白作业并返回作业错误。
- 保留有/无语言标签的 job-name，并传给 Windows 队列。新增 print-scaling 的真实执行：fit、fill、none、auto、auto-fit；自动模式在页面大于纸张或要求属性保真时按可打印区域适配，否则保留原始尺寸。PDF 原始尺寸按明确的 300 DPI 渲染分辨率计算。
- Create-Job 的打印选项在 Send-Document 中继承；增加协议到后端的意图保留测试、非法范围检查，以及 `scripts/check_print_options.ps1` 的页面选择和缩放检查（不访问打印机）。
- 接收日志包含所选选项，Windows 投递失败记录具体错误；没有将此次修改描述为已修复手机系统 AirPrint 失败，仍需实际设备验证。
- 本轮验证：库测试 18 passed / 0 failed / 4 ignored；适配器参数测试 1 passed。另单独运行四格式 Windows 虚拟打印及本地 HTTP→Windows→作业完成测试，各 1 passed。`check_print_options.ps1 -Simulate` 验证三页输出和选择第 2 页后的一页 PDF，未出现保存或打印对话框；默认模式验证页码及全部公布的缩放计算。
- Windows 调试信息链接耗时较长，验证使用 `target/test-rustc-no-pdb.cmd` 临时包装编译器追加 `/DEBUG:NONE`；仅用于本机测试/构建，未改变项目依赖和 Cargo 配置。上述结果没有使用实体打印机或实际 Apple 系统打印客户端。
- 更新后的可运行程序已构建：`cargo build --bin airprinter --features custom-protocol --locked --offline`（同一临时包装脚本）。输出 `src-tauri/target/debug/airprinter.exe`，包含现有前端与本轮自动处理逻辑；未启动程序或修改共享、防火墙设置。

## 手机系统打印失败的实机诊断（2026-10-10）

- 用户再次从手机系统打印面板尝试，仍未成功。当前共享为 USB005 上的 `FF Q501h for DocuPrint P115 b`；检查时 Windows 队列为空，当前 IPP 作业列表也为空。此状态提示失败可能发生在作业接受之前，但缺少历史请求记录，不能确定根因。
- 新增自动诊断：启动后将请求路径、对端、HTTP 头中的传输信息、接收字节数、IPP 操作/返回码、URI 校验失败、解析错误和 Windows 投递/状态错误写入应用配置目录 `print-diagnostics.jsonl`。只记录元数据及错误，不保存文档内容或 TLS 私钥；每份日志限制约 2 MiB，保留一份轮换记录。
- 新增记录格式与容量轮换测试；本轮库测试 19 passed / 0 failed / 4 ignored，内嵌前端的程序构建通过。已启动诊断版本，恢复已有共享选择，HTTP 631 / HTTPS 8631 在线。下一次手机提交将由电脑自动记录，仍待实机结果确认。
- 随后的实机记录：手机通过 HTTP、Transfer-Encoding: chunked 成功提交多次 Get-Printer-Attributes（0x000b），均返回 successful-ok；没有 Print-Job/Create-Job/Send-Document，文件尚未上传。排查阶段因此缩小到打印前能力协商，不能再将这一现象笼统归因于文件上传或 Windows 打印驱动。
- 对照 PWG 5100.13-2023 第 44–46、56–61 页，补齐声明 PDF/JPEG 后条件必需的大小/特性/尺寸属性；给自动驱动选择提供对应的输入/输出纸盒元数据及 media-col 的 source/type。容量及纸张余量未知，用规范的 unknown/-2 表示，不虚构实体传感器读数。output-bin 只接受 auto；其他值返回不支持。
- 已增加手机查询字段的序列化回放测试，并验证公布的纸张集合能通过 Validate-Job；库测试 20 passed / 0 failed / 4 ignored。这些修复针对可验证的能力属性缺口，是否解决实机停止在查询阶段仍待复测。
- 用户确认来源为文件 App 的 PDF，无手机报错。补齐属性后的复测仍停在查询阶段；临时恢复旧版完整能力响应也未使手机提交文件，已撤回完整响应实验。
- 对比 699e9e7 又发现 URI 行为差异：旧版收到 `.local` Host 会改用本机 IPv4 返回 printer-uri；新版本直接回显 `.local` Host 且可能省略默认端口。已恢复 HTTP 的 IPv4 兼容路径：用手机对端的 UDP 路由选择得到本机源地址（只 connect，不发送数据），避免选到虚拟网卡，返回明确端口。IPPS 保留 TLS 证书匹配的 DNS 主机名。
- 手机实际查询列表不请求 printer-uri，因此能力响应同时附带规范化的提交 URI 及对应的 authentication/security 属性，其余仍按 requested-attributes 筛选。新增路由地址、端口、Bonjour 请求与作业 URI 回归测试：21 passed / 0 failed / 4 ignored。此差异是进一步定位线索，实机打印结果仍待确认。

## 原始抓包与仅接收阶段（2026-10-10）

- 按用户要求由 GPT-6 Luna 编写抓包、分析和仅接收代码，主代理审阅并验收。捕获产物在 `target/print-capture/20261010-182646/`，包含原始网络 ETL、PCAPNG、跨层包文本、PrintService/USBMon ETW、175 次队列采样与应用记录；产物目录已加入 Git 忽略。
- 抓包实际从 18:26:47 开始，采样及停止处理至 18:28:23。可解码的明文请求为 Get-Printer-Attributes，可解码响应为 HTTP 200 / IPP successful-ok。部分 TCP 流有捕获缺口，不能把未配对的请求、响应当作同一交易。所有队列采样为空；两份打印 ETW 解码后只有会话头，无打印/USBMon事件。USBMon是驱动事件跟踪，不是原始 USB payload。
- PktMon 的 1153 条跨层 drop 记录里，1148 条与 UDP 5353 有关；5 条 TCP 631 分别是 duplicate segment/FIN-WAIT2。它们不是1153次打印TCP丢包，也不是捕获ETW事件丢失。
- 关键原始报文：18:27:39 手机 `192.168.3.197:60841` 向 `192.168.3.249:631` 发送完整 TLS ClientHello（record header `16030105e9`、记录长1513字节）；无 SNI，支持 TLS1.3/1.2。服务器只有TCP ACK，未见 ServerHello/证书/TLS应用数据；手机随后 FIN。当前631为明文HTTP，TLS只在8631。用户确认捕获期间只操作文件 App 的 PDF 系统打印，没有打开浏览器。此时不能判断证书信任是否存在后续问题，因为尚未到证书阶段。
- 阶段目标改为先收到文件。新增本地启动参数 `--receive-only <absolute-directory>`，接收文件后保存完整payload，标记 held（诊断保存，不代表已打印），不调用 Windows 后端；正常启动的自动打印逻辑保持原样。协议回归22 passed / 0 failed / 4 ignored。
- 本机对照：仅接收版本使用 chunked HTTP/IPP 接收5003字节的本机生成PDF，保存内容逐字节一致，SHA256 `523a225391931e7c8d18ae6e6548084e3b97e0f4a81edf5867328b0fbf457be6`，spool_id为空，实体Windows队列无新增作业。这项结果是本机对照，不是手机文件接收成功。
- 依据 TLS ClientHello 发往631的原始证据，增加同端口 HTTP/TLS 接入层：按首字节分流到回环HTTP后端或现有TLS后端，不落盘转发内容；上限16连接，设置超时，并通过短生命周期映射恢复真实手机地址及公开端口。协议/代理回环及实际本机HTTPS631上传均通过。
- 修复后第二轮捕获 `target/print-capture/20261010-184317/` 显示 TLS1.3 握手及加密请求，应用明确记录 `secure=true POST /ipp/print` 被404拒绝。增加仅单共享队列可用的默认路径别名；多队列时仍拒绝，响应和作业继续绑定canonical路径。第三轮 `20261010-184917` 再发现手机使用Host `LicherPC.local`，因此允许当前本机 `COMPUTERNAME.local`，保留陌生主机名及错误端口拒绝。这些修复后系统打印仍只查询能力，没有观察到文档提交；没有将其宣称为系统AirPrint已完全修复。
- 用户批准先从网页上传验证接收。新增 `/receive`，只在仅接收模式且恰好共享一台队列时开放；使用同一个IPP接收器上传PDF/JPEG，按精确作业ID查询保存结果，不下载素材、不触发实体打印。网页使用 `X-AirPrinter-Receive-Only: 1`，服务端在普通模式拒绝该标记，防止旧网页在重启为打印模式后误打印。
- **真实手机接收验收通过（19:00:20）**：手机 `192.168.3.197` 从网页提交 `symbols.pdf`，Print-Job文档为 `application/pdf`，261478字节；接收文件 `src-tauri/target/received-documents/airprinter-job-2-I3h75t.pdf` 大小与接收记录一致，Windows PDF API可解析为4页。SHA256为 `AF0440C4A882559C3F1B227F62A3B8FD28044ECA181CD5287431113D9C45A4E4`。作业held、spool_id为空，Windows实体队列没有新增任务。本阶段“先完整收到手机文件”已实现；系统打印面板原生提交与实体输出留待下一阶段。

## 统一虚拟入口与实体队列路由（2026-10-10）

- 按用户要求，由 GPT-6 Luna 实现固定虚拟 AirPrinter，主代理审核与验收。PC 选择一个实际 Windows 输出队列；网络只发布一个虚拟打印机身份，公共路径固定为 `/ipp/print`，实例名和 UUID 不随目标队列切换。无需安装 Windows 虚拟打印驱动。
- 打印能力来自选定的实际队列。Create-Job/Print-Job 接受时保存实际队列与能力快照；切换后新作业使用新队列，后续 Send-Document 仍使用原作业的队列、打印选项和能力。取消及状态查询也使用作业保存的实际队列。
- 前端改为单选输出目标。切换通过一次后端请求更新，保存失败不会切换，运行时更新失败恢复原选择；启动时不会在多个旧共享记录中任意选择目标，需重新单选。
- 普通模式自动接收、渲染并投递 Windows 队列，临时文件自动清理；仅接收诊断模式仍按明确启动参数保存文件且不打印。此前收到的 `symbols.pdf` 不会因正常重启而自动重放。
- 验证：库测试 33 passed / 0 failed / 4 ignored；前端 TypeScript/Vite 构建和 Rust all-targets 检查通过。单独运行 HTTP 虚拟 `/ipp/print` → 实际 `Microsoft Print to PDF` 测试，Windows spool id 10，IPP 作业最终 completed；输出 `src-tauri/target/airprint-simulation/http-urf-46196.pdf`。目标切换测试覆盖 A/B 队列和 Create-Job 后切换再 Send-Document 的目标保持。
- 这些结果证明统一入口与 Windows 路由实现可运行。真实手机系统打印面板在新虚拟身份下提交文件，以及 FF Q501h 实体出纸仍需实机验证；网页接收成功不能替代这两项验收。
- 新版已于 19:23:35 正常启动（无 receive-only 参数），恢复 `FF Q501h for DocuPrint P115 b`。HTTP/HTTPS 631 查询都返回 `printer-name=AirPrinter`、`printer-is-accepting-jobs=true` 和固定 `/ipp/print`，普通模式 `/receive` 为404。只读查询未提交文档。
- 本机临时端口的单播 DNS-SD 查询未收到响应，改为加入5353多播接收后观察到实际 PTR/SRV/TXT/A：`AirPrinter-b90176c7458058b3`，`rp=ipp/print`，UUID `881a6a07-2eef-55ce-ba76-36b91b572323`；IPP631/IPPS8631使用同一虚拟身份，A记录 `192.168.3.249`。这是实际广播记录证据，手机是否发现及提交仍待用户实测。

## 虚拟入口上线后的旧地址兼容排查（2026-10-10）

- 用户确认已发现 AirPrinter，提交后仍未出纸。19:24–19:26 日志显示手机对 `/ipp/print` 的 TLS能力查询成功，同时持续对 `/ipp/print/0446aacd44605763b86a4d6fa188cd7d` 发起HTTP请求被404拒绝；Windows实体队列没有新增作业，已解析请求仍只有Get-Printer-Attributes。
- 旧路径后缀与当前 `FF Q501h for DocuPrint P115 b` 队列名的 UUID v5完全一致。这支持手机仍在访问旧队列地址的判断，但不证明404是所有原生提交失败的唯一原因。兼容修复仅针对当前实体目标的精确旧路径，统一返回虚拟入口；不接受任意UUID或其他实体队列。
- 已完成精确别名兼容：HTTP路由解析及IPP处理读取目标时都检查当前路由，避免两步之间切换导致串队列；旧路径提交和作业查询统一返回 `/ipp/print` 的URI。新增真实HTTP回归覆盖文档/实体目标、错误UUID、未选择队列、切换后的旧地址及停止后的拒绝。
- 本轮库测试在正常主机权限下为34 passed / 0 failed / 4 ignored。初次沙箱内运行有10项被套接字/临时文件访问限制阻止；直接在正常主机权限下运行同一测试二进制全部通过，没有修改实现或放宽校验来绕过测试。
- 兼容版于19:31:03启动，正常自动打印模式、目标队列不变。实机只读查询验证精确旧地址在HTTP及HTTPS631均返回HTTP200/IPP successful-ok，响应`printer-name=AirPrinter`，提交URI统一为`/ipp/print`；错误UUID仍为404。真实手机复测结果待确认。

## IPP Everywhere 1.1 规范符合性补全（2026-10-11，virtual-gateway-printer 分支）

- 诊断日志（5478条）证实手机 660 次请求全部是 Get-Printer-Attributes 且均 successful-ok；系统打印面板从未发出 Print-Job/Create-Job/Send-Document（4 次 Print-Job 均为本机或网页对照）。手机固定索取但我们没有的属性包括 `printer-mandatory-job-attributes`（每次轮询）、`printer-more-info`、`printer-location`、`printer-dns-sd-name`、`printer-strings-uri`、`printer-supply-info-uri`、`identify-actions-supported` 等。
- 对照 `doc/cs-ippeve11-20200515-5100.14.md` 表4/5/7/8/10/11/12 补齐：
  - 操作：新增 Close-Job (0x003B)、Cancel-My-Jobs (0x0039)、Identify-Printer (0x003C)，`operations-supported` 同步声明；Close-Job 将空作业标记完成并返回完整作业属性。
  - 表5：补 `media-size-supported`、四个 `media-*-margin-supported`、`printer-location`、`printer-organization(-unit)`、`printer-geo-location`（no-value）、`printer-get-attributes-supported`、`printer-icons`（新增 /icons/*.png HTTP端点，绝对URI）、`printer-more-info`（新增 / 信息页）、`job-ids-supported`、`job-constraints/resolvers-supported`（no-value）、`preferred-attributes-supported`、`multiple-operation-timeout-action`、`overrides-supported`、`identify-actions-*`、`printer-mandatory-job-attributes`（no-value）、`printer-dns-sd-name`（与 mDNS 实例名同一计算来源）。
  - 表7：补 `pages-per-minute(-color)`、`printer-config-change-date-time/-time`、`printer-state-change-date-time/-time`、`printer-current-time`（UTC dateTime）。
  - 表10/11：作业组补 `job-uuid`（v5，含进程启动时刻避免复用）、`time-at-creation/-processing/-completed`、`date-time-at-*`（未到达的时点用 no-value）、`job-printer-up-time`、`job-impressions(-completed)`。
  - 表8/12：`options()` 现在处理 `document-name`（无 job-name 时作为作业名）、`printer-resolution`（仅接受已声明的 300dpi）、`multiple-document-handling`（仅 collated）；`media-col-supported` 列出全部已用成员（含四个边距）。
  - DNS-SD：URF 键与 `urf-supported` 改为按驱动能力动态生成（双面加 DM1、多份加 CP1）；IPP/IPPS 服务新增 `adminurl`；`ty` 截断收紧到64字节以保持 TXT ≤400 字节（测试守护）。
- 已知遗留偏差：mdns-sd 0.11 每个实例只能注册一个子类型，无法同时发布规范 4.2.1 要求的 `_print._sub._ipp._tcp`（保留 Apple 实际使用的 `_universal._sub`，手机发现已被日志证实）；4.2.3 的 LOC 记录 mdns-sd 不支持；`printer-strings-uri`（RECOMMENDED）与耗材属性（条件必需）继续省略。
- 验证：库测试 36 passed / 0 failed / 4 ignored（新增 `conformance_attribute_tables_and_operations` 与 `close_job_cancel_my_jobs_and_identify_printer`，响应经线上格式序列化回放校验）；`cargo check --all-targets` 通过。手机系统打印面板是否因此进入提交阶段仍需实机复测。

## 规范复审与双方案分支（2026-10-12）

- 两个方案分别在 `direct-printer-broadcast` 与 `virtual-gateway-printer`；共同 IPP/DNS-SD 层的代码相同，但直接广播按队列独立 URI，虚拟入口按接收时的目标快照路由。条款、验收门槛和未完成项见 `conformance-plan.md`。
- 复查 PWG 5100.14 §4.2.2：DNS-SD 实例名默认不再附加队列 ID 哈希。发布前拒绝与本进程现有实例同名的队列，避免 `mdns-sd` 按 fullname 覆盖旧记录；碰撞后自动改名尚未实现。
- 复查 §5.1：所有 HTTP 请求入口统一验证唯一 Host；静态图标支持 Last-Modified、If-Modified-Since 和 304，IPP 正常 POST 响应维持 `Cache-Control: no-cache`。
- 复查 §7.1：PC 软件桥接器按逻辑设备发布 `ipp-everywhere-server`。IPP `printer-make-and-model` 与 DNS-SD `ty`/`product` 统一为软件型号 `AirPrinter Windows Print Bridge`，不再把 Windows 队列名当作实体型号。未知的速度和可打印边距使用 IPP `unknown` (0x12)，不再发布未经测量的 1 ppm / 6.35 mm；未配置的值仍按语义使用 `no-value`。
- 复查 §9.3：此前 `overrides-supported` 声称支持页级覆盖，但作业流程没有执行它。现已撤销该虚假声明并拒绝客户端 `overrides`；非空 `document-password` 同样明确拒绝，不再静默忽略。**PDF 条件必需的 overrides 仍未实现，故不符合完整 IPP Everywhere 1.1。**
- 当前 `mdns-sd 0.11.5` 每个服务实例只能持有一个 subtype；对相同 fullname 再注册会替换已有记录。因此 `_universal` 之外的 `_print` 子类型及 §4.2.3 LOC/手动位置配置仍未完成，不能以属性表测试宣称完整规范符合性。
- 直接广播分支本轮验证：Rust 库测试 **38 passed / 0 failed / 4 ignored**；`cargo check --all-targets --locked` 与 `pnpm build` 通过。测试中的回环监听使用正常主机权限；没有从真实 Apple 系统打印面板完成文件提交或实体出纸，本轮不发布正式版本。
- 共同修复移植到 `virtual-gateway-printer` 后，Rust 库测试 **40 passed / 0 failed / 4 ignored**；`cargo check --all-targets --locked` 与 `pnpm build` 通过。固定虚拟入口的目标切换和旧作业绑定测试仍通过；尚未取得真实 Apple 原生提交及实体出纸证据。

## 直接广播实机成功与 1.0.0 快照（2026-10-12）

- 用户确认 `direct-printer-broadcast` 已从 Apple 系统打印流程成功输出到实体打印机。该成功路径快进合并到 `main`，并以注释标签 `v1.0.0` 固定；后续后台生命周期和界面改造从该标签之后继续。
- 开机启动注册命令改为 `"AirPrinter.exe" --background`。后台启动不创建 WebView；普通窗口关闭到托盘时销毁 WebView，点击托盘再按配置创建。实测后台无 AirPrinter 窗口，调试构建工作集约 22 MB、私有内存约 4 MB。
- Windows 队列能力检测在前台保持 30 秒周期，后台无窗口时降为 5 分钟；应用启动后仍立即执行一次同步，保证已保存的共享恢复。
- 界面改为 820×506（约 1.62:1）、最小 720×445 的无边框横向窗口，左侧导航切换打印机、日志和设置。真实 Tauri 窗口、三页导航、打印机状态和共享开关完成目视验收。
