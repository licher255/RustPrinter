# AirPrinter

[查看 HTML 项目文档](doc/index.html)

本分支 `direct-printer-broadcast` 将 PC 上已安装驱动的 USB、有线网络或已配对 Windows 队列分别发布到局域网。每个共享队列有独立的发现身份和 IPP 资源路径，收到的作业直接投递到对应的 Windows 队列。固定虚拟入口的方案在 `virtual-gateway-printer` 分支。

## 使用

1. 在 Windows 安装打印机驱动，并确认本机可以正常打印。
2. 启动 AirPrinter，在列表中分别开启需要共享的 Windows 打印队列。
3. 让 iPhone/iPad/Mac 与 PC 位于可以互通的局域网，在系统打印面板中选择对应队列。
4. 可单独停止某台队列的共享；另一台仍保持服务。已接收作业继续绑定其原队列。

日常打印在手机系统打印面板选择已共享的队列、设置打印选项并点击打印。PC 自动接收、处理和投递到该 Windows 队列；无需下载素材、打开查看器或再次确认打印。浏览器测试页仅用于诊断连接，不是日常打印步骤。

PDF/XPS/OneNote/Fax 等需要本地交互的虚拟队列不出现在正式共享列表中。模拟测试会显式使用 Microsoft Print to PDF，自动指定输出文件，不需要保存对话框。

## 当前链路

```text
Windows 队列与驱动能力
  → 每队列独立的 DNS-SD 服务与 /ipp/print/<id> 入口
  → IPP Print-Job 或 Create-Job + Send-Document
  → 按资源路径绑定对应的 Windows 队列
  → PDF / JPEG / URF / PWG Raster 页面渲染
  → 指定 Windows 队列的 GDI / Spooler
  → IPP 查询、取消和作业状态
```

- 使用 Rust `mdns-sd` 为每个共享队列广播 `_ipp._tcp`、`_universal._sub._ipp._tcp` 和端口为 0 的 `_printer._tcp` 标识服务。当前规范子类型与 LOC 偏差见[符合性计划](doc/conformance-plan.md)。
- 每台队列有独立的 `/ipp/print/<id>` URI；只有恰好共享一台时才接受 `/ipp/print` 兼容别名。
- 遵循 IPP 的默认端口语义：`ipp://` URI 可以省略 631，HTTP `Host` 仍按实际监听端口验证；Bonjour `.local` 名称和本机 IP 都可作为合法目标。
- 每 30 秒同步 Windows 队列状态与驱动能力。单个 Windows 输出目标的选择保存在用户配置目录，不复制待打印文档。
- PDF 使用 Windows 自带 `Windows.Data.Pdf`；JPEG 使用 Windows 图像解码；URF/PWG Raster 支持所公布的 sGray8 / sRGB24，经 GDI 静默投递。
- 支持 Print-Job、Validate-Job、Create-Job、Send-Document（单文档）、Get-Job-Attributes、Get-Jobs、Cancel-Job。
- 公布驱动实际支持的彩色、双面、份数和纸张尺寸；A4、A5、Letter、Legal 使用标准名称，其余驱动纸型按尺寸生成自定义介质名称。
- 彩色打印机默认按 Windows 驱动能力输出彩色；IPP 的 `which-jobs-supported` 与 `multiple-operation-timeout` 属性使用规范名称，DNS-SD TXT 保持在建议的 400 字节以内。
- 支持横竖方向、连续或不连续的页码范围，以及适合页面、填满、原始尺寸和自动缩放；页码按收到的文档从 1 开始编号。PDF 只渲染所选页面，JPEG/URF/PWG Raster 按页选择；不宣称无边距、装订或多文档作业能力。
- 手机提交的作业名称及份数、双面、彩色、纸张、质量、方向、页码和缩放设置进入同一后台打印流程；Create-Job 中的设置在 Send-Document 到达时保留。原始尺寸或填满模式超出可打印区域的部分会被裁切，非法页码范围或驱动不支持的选项返回错误。
- 接收作业时返回待处理状态。驱动失败返回 aborted；进入 Windows 队列后跟踪其状态。作业离开 Windows 队列表示后台队列处理结束，不能独立证明纸张已经输出。
- 文档暂存于随机临时目录，提交结束或失败后清理；不打开浏览器、默认查看器或打印对话框，也不更改 Windows 默认打印机。

## IPP 与 AirPrint 的关系

发现使用 DNS-SD/mDNS，作业传输使用 IPP/HTTP。URF 是为了 Apple 客户端互操作实现的文档格式，不是替代 IPP 的私有传输协议；同时实现了 `doc/cs-ippeve11-20200515-5100.14.pdf` 中的 PWG Raster 格式路径。

本项目目前**不宣称已通过完整 IPP Everywhere 1.1 认证，也不保证所有 Apple 系统版本**。`_print._sub._ipp._tcp` 和 `ipp-features-supported=ipp-everywhere` 不应仅为让设备出现而虚假声明；现实现还有完整规范的扩展操作、属性和认证测试待补。Apple 系统打印面板与真实硬件仍需实机互操作验收。

- [IPP Everywhere / PWG](https://www.pwg.org/ipp/everywhere.html)
- [Apple 关于 AirPrint DNS-SD 子类型与 URF 的说明](https://support.apple.com/guide/deployment/dep3b4cf515/web)
- [RFC 6763 DNS-SD](https://www.rfc-editor.org/rfc/rfc6763)

## 网络与防火墙

当前 HTTP 优先监听 IPv4 `0.0.0.0:631`。如果 Windows 暂时保留了此端口，程序会改用 8631–8699 中的空闲端口，并在 DNS-SD SRV 记录中广播实际端口；防火墙脚本覆盖这两个端口范围。广播可用 IPv4 接口的地址，不发布不可达的 IPv6 服务。PC 可以通过网线连接路由器，Apple 设备使用 Wi-Fi，只要局域网允许相互通信和 UDP 5353 组播。访客网络、AP 隔离或 VLAN 隔离需要单独处理。

监听 TCP 631 本身不要求 Windows 管理员权限。防火墙放行需要权限。应用中的“允许局域网访问”按钮会请求 Windows 管理员授权，并为本程序建立仅限本地子网的 TCP/UDP 入站规则，覆盖 Private 和 Public 网络配置。也可以在管理员 PowerShell 中对实际运行的 EXE 手动执行：

```powershell
.\scripts\enable_firewall.ps1 -Program (Resolve-Path .\src-tauri\target\debug\airprinter.exe)
```

正式安装后将路径替换为已安装 EXE。程序只在用户主动点击按钮或手动执行脚本后修改防火墙。IPP 无身份验证；在 Public 网络上开启后，同一子网的其他设备也可提交打印作业，请只在可信局域网使用。

## 构建

```powershell
pnpm install
pnpm tauri dev
pnpm tauri build
```

前端独立构建：`pnpm build`。如果新版 pnpm 因已有依赖目录的校验试图重装，可用 `pnpm --config.verify-deps-before-run=false run build` 检查现有依赖。

后端使用系统 Windows PowerShell、.NET Framework 的 System.Drawing、Windows PDF API 及已安装打印驱动，不要求额外 PDF 阅读器。

## 可重复验证

定位手机文件传输时，可先启动仅接收诊断模式：`src-tauri/target/debug/airprinter.exe --receive-only "E:\Tauri-Rust-Printer\src-tauri\target\received-documents"`（目录必须为绝对路径）。已接收的文档保存在该目录，作业标为待处理，暂不调用打印驱动。退出后正常启动恢复自动打印。此模式用于接收验收，不是日常使用步骤。

仅接收诊断模式可从手机访问 `http://<PC局域网地址>:631/receive`（或同端口 HTTPS），上传一个测试文档并将作业保存为待处理状态，不调用打印驱动。该入口用于诊断接收链路，不是日常打印流程。普通模式会自动处理并投递作业，无需用户下载文档、打开查看器或再次手动打印。

Windows 管理员 PowerShell 中运行 `scripts/capture_print_chain.ps1 -OutputDirectory "E:\Tauri-Rust-Printer"` 可进行90秒限定抓包，同步记录手机631/8631流量、本子网mDNS、打印/USBMon事件及队列状态；产物在 `target/print-capture/`。用 `python -X utf8 scripts/analyze_print_capture.py <network.pcapng>` 分析原始报文。USBMon产物是驱动事件而非原始USB数据包。抓包可能含文档数据，只在本地保存并已从Git忽略。

```powershell
# 协议、解析与路由测试
cargo test --manifest-path src-tauri/Cargo.toml --locked
python scripts/diagnose_mdns.py --self-test
powershell.exe -NoProfile -File scripts/check_print_options.ps1
# Windows 自动选页集成测试：只向 Microsoft Print to PDF 输出，无保存对话框
powershell.exe -NoProfile -File scripts/check_print_options.ps1 -Simulate

# 本地模拟：使用 Microsoft Print to PDF 生成文件
cargo test --manifest-path src-tauri/Cargo.toml --locked -- --include-ignored --nocapture

# JS 虚拟客户端与主机专用虚拟网卡：不会提交 Print-Job
cargo build --manifest-path src-tauri/Cargo.toml --example simulate_lan --locked
node scripts/simulate_airprint.mjs 172.22.48.1

# 从另一台局域网电脑检查真实广播；不会提交打印作业
python scripts/diagnose_mdns.py --seconds 8
# 多网卡时指定本机局域网接口地址
python scripts/diagnose_mdns.py --interface 192.168.1.10 --seconds 8
```

既有本机测试覆盖独立队列路由、IPP 属性与虚拟打印输出；当前改动的测试结果以本轮审计记录为准。该验证不代表真实手机原生提交成功；真实 Apple 设备提交和实体打印机出纸仍未验证。

`http_to_windows_pdf_end_to_end` 通过 HTTP IPP 测试入口提交 URF，查询作业直到 Windows 队列结束，在 `src-tauri/target/airprint-simulation/` 留下 PDF 供检查。`windows_raster_spool_smoke` 检查四种文档格式，包括 PDF 再渲染打印。mDNS 测试验证解析和注销。

JS 脚本需要一块主机专用虚拟网卡，参数应替换为该网卡的 IPv4 地址。Rust 模拟打印机把 IPP 监听及 mDNS 限定在该接口；Node.js 在另一进程中模拟 DNS-SD 与 IPP 客户端。这台电脑的 Hyper-V 默认交换机不会将 mDNS 多播回送本机，因此脚本会明确标记 DNS-SD 部分使用隔离的 UDP 回环模拟响应。IPP 属性查询、参数校验与 Host 拒绝仍访问真实 Rust 后端。它不会向虚构队列提交打印作业。

这些测试证明本机实现链路可运行，不替代 Wi-Fi 路由器、防火墙、Apple 系统打印界面和实体打印机的联合验收。资源上限为单次上传 64 MiB、光栅解码累计 512 MiB、最多 500 页、单次 Windows 渲染/投递 120 秒、4 个正在处理的作业、32 个未结束作业；超过上限返回错误，不无限增长。

[许可证](LICENSE)
