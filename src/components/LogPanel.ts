import { logService, LogEntry } from "../services/logService";
import * as diagnostics from "../services/diagnosticsService";
import i18n from "../i18n";

type LogTab = "session" | "diagnostics";

export class LogPanel {
  private container: HTMLElement;
  private logArea: HTMLElement;
  private clearBtn: HTMLButtonElement | null;
  private refreshBtn: HTMLButtonElement | null;
  private copyBtn: HTMLButtonElement | null;
  private openFolderBtn: HTMLButtonElement | null;
  private tabSessionBtn: HTMLButtonElement | null;
  private tabDiagnosticsBtn: HTMLButtonElement | null;
  private titleEl: HTMLElement | null;
  private pathBar: HTMLElement | null;
  private activeTab: LogTab = "session";
  private diagnosticsText = "";

  private handleLanguageChange = () => {
    this.updateStaticLabels();
    if (this.activeTab === "diagnostics") {
      this.renderDiagnostics();
    } else {
      const currentLogs = logService.getLogs();
      if (!currentLogs || currentLogs.length === 0) {
        this.update([]);
      }
    }
  };

  constructor(containerId: string) {
    const container = document.getElementById(containerId);
    if (!container) throw new Error(`找不到元素: ${containerId}`);
    this.container = container;

    this.render();

    this.logArea = this.container.querySelector("#log-content")!;
    this.clearBtn = this.container.querySelector("#clear-log");
    this.refreshBtn = this.container.querySelector("#lp-refresh");
    this.copyBtn = this.container.querySelector("#lp-copy");
    this.openFolderBtn = this.container.querySelector("#lp-open-folder");
    this.tabSessionBtn = this.container.querySelector("#lp-tab-session");
    this.tabDiagnosticsBtn = this.container.querySelector("#lp-tab-diagnostics");
    this.titleEl = this.container.querySelector("#lp-title");
    this.pathBar = this.container.querySelector("#lp-path");

    this.clearBtn?.addEventListener("click", () => {
      logService.clear();
    });
    this.refreshBtn?.addEventListener("click", () => {
      void this.loadDiagnostics();
    });
    this.copyBtn?.addEventListener("click", () => {
      void this.copyCurrentLog();
    });
    this.openFolderBtn?.addEventListener("click", () => {
      void this.openLogFolder();
    });
    this.tabSessionBtn?.addEventListener("click", () => this.switchTab("session"));
    this.tabDiagnosticsBtn?.addEventListener("click", () => this.switchTab("diagnostics"));

    i18n.on("languageChanged", this.handleLanguageChange);
    this.updateStaticLabels();
    void this.loadLogPath();

    logService.onUpdate((logs) => {
      if (this.activeTab === "session") this.update(logs);
    });
  }

  public destroy() {
    i18n.off("languageChanged", this.handleLanguageChange);
  }

  private render() {
    this.container.innerHTML = `
      <section class="panel">
        <div class="panel-header">
          <div class="panel-title-group">
            <span class="panel-icon"><i class="fa-solid fa-terminal"></i></span>
            <h2 id="lp-title" class="panel-title"></h2>
          </div>
          <div class="lp-toolbar">
            <div class="lp-tabs" role="tablist">
              <button id="lp-tab-session" class="lp-tab active" type="button" role="tab"></button>
              <button id="lp-tab-diagnostics" class="lp-tab" type="button" role="tab"></button>
            </div>
            <button id="lp-refresh" class="btn-icon" type="button" hidden>
              <i class="fa-solid fa-rotate" aria-hidden="true"></i>
            </button>
            <button id="lp-copy" class="btn-icon" type="button">
              <i class="fa-solid fa-copy" aria-hidden="true"></i>
            </button>
            <button id="lp-open-folder" class="btn-icon" type="button">
              <i class="fa-solid fa-folder-open" aria-hidden="true"></i>
            </button>
            <button id="clear-log" class="btn-icon" type="button">
              <i class="fa-solid fa-trash-can" aria-hidden="true"></i>
            </button>
          </div>
        </div>
        <div id="lp-path" class="log-path" hidden></div>
        <div id="log-content" class="log-area">
          <div class="hint"></div>
        </div>
      </section>
    `;
  }

  private updateStaticLabels() {
    if (this.titleEl) {
      this.titleEl.textContent = i18n.t("logs.title");
    }
    const labelled: [HTMLElement | null, string][] = [
      [this.clearBtn, "logs.clear"],
      [this.refreshBtn, "logs.refresh"],
      [this.copyBtn, "logs.copy"],
      [this.openFolderBtn, "logs.open_folder"],
    ];
    for (const [el, key] of labelled) {
      el?.setAttribute("title", i18n.t(key));
      el?.setAttribute("aria-label", i18n.t(key));
    }
    if (this.tabSessionBtn) this.tabSessionBtn.textContent = i18n.t("logs.tab_session");
    if (this.tabDiagnosticsBtn)
      this.tabDiagnosticsBtn.textContent = i18n.t("logs.tab_diagnostics");
  }

  private switchTab(tab: LogTab) {
    this.activeTab = tab;
    this.tabSessionBtn?.classList.toggle("active", tab === "session");
    this.tabDiagnosticsBtn?.classList.toggle("active", tab === "diagnostics");
    if (this.clearBtn) this.clearBtn.hidden = tab !== "session";
    if (this.refreshBtn) this.refreshBtn.hidden = tab !== "diagnostics";
    if (tab === "diagnostics") {
      void this.loadDiagnostics();
    } else {
      this.update(logService.getLogs());
    }
  }

  private async loadLogPath() {
    try {
      const info = await diagnostics.getLogInfo();
      if (this.pathBar) {
        this.pathBar.hidden = false;
        this.pathBar.textContent = i18n.t("logs.path_label", {
          path: info.path,
          size: this.formatSize(info.size_bytes),
        });
      }
    } catch {
      // Diagnostics not initialized (e.g. pure-browser dev preview): keep the bar hidden.
    }
  }

  private async loadDiagnostics() {
    this.logArea.innerHTML = `<div class="hint">${i18n.t("logs.diagnostics_loading")}</div>`;
    try {
      this.diagnosticsText = await diagnostics.readLogs();
      await this.loadLogPath();
      this.renderDiagnostics();
    } catch (error) {
      this.logArea.innerHTML = `<div class="hint">${this.escapeHtml(
        i18n.t("logs.diagnostics_failed", { error: String(error) })
      )}</div>`;
    }
  }

  private renderDiagnostics() {
    if (!this.diagnosticsText.trim()) {
      this.logArea.innerHTML = `<div class="hint">${i18n.t("logs.diagnostics_empty")}</div>`;
      return;
    }
    const pre = document.createElement("pre");
    pre.className = "log-diagnostics";
    pre.textContent = this.diagnosticsText;
    this.logArea.replaceChildren(pre);
    this.logArea.scrollTop = this.logArea.scrollHeight;
  }

  private sessionLogsAsText(logs: LogEntry[]): string {
    return logs.map((log) => `[${log.time}] [${log.level}] ${log.message}`).join("\n");
  }

  private async copyCurrentLog() {
    const text =
      this.activeTab === "diagnostics"
        ? this.diagnosticsText
        : this.sessionLogsAsText(logService.getLogs());
    if (!text.trim()) return;
    try {
      await this.copyText(text);
      this.flashCopyFeedback(true);
    } catch (error) {
      this.flashCopyFeedback(false);
      logService.add(i18n.t("logs.copy_failed", { error: String(error) }), "error");
    }
  }

  private async copyText(text: string): Promise<void> {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return;
    }
    // Fallback for contexts where the async clipboard API is unavailable.
    const area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    try {
      document.execCommand("copy");
    } finally {
      area.remove();
    }
  }

  private flashCopyFeedback(ok: boolean) {
    const icon = this.copyBtn?.querySelector("i");
    if (!icon) return;
    const original = icon.className;
    icon.className = ok ? "fa-solid fa-check" : "fa-solid fa-xmark";
    if (ok) icon.style.color = "#34c759";
    window.setTimeout(() => {
      icon.className = original;
      icon.style.color = "";
    }, 1500);
  }

  private async openLogFolder() {
    try {
      await diagnostics.openLogFolder();
    } catch (error) {
      logService.add(i18n.t("logs.open_folder_failed", { error: String(error) }), "error");
      alert(i18n.t("logs.open_folder_failed", { error: String(error) }));
    }
  }

  private formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  private update(logs: LogEntry[]) {
    if (logs.length === 0) {
      this.logArea.innerHTML = `<div class="hint">${i18n.t("logs.waiting")}</div>`;
      return;
    }

    this.logArea.innerHTML = logs
      .map((log) => {
        const color = {
          info: "#0a84ff",
          success: "#34c759",
          error: "#ff3b30",
          warning: "#ff9500",
        }[log.level] || "#86868b";

        return `
        <div style="margin-bottom: 2px; word-break: break-all;">
          <span style="color:#aeaeb2; margin-right:8px;">[${log.time}]</span>
          <span style="color:${color};">${this.escapeHtml(log.message)}</span>
        </div>
      `;
      })
      .join("");

    this.logArea.scrollTop = this.logArea.scrollHeight;
  }

  private escapeHtml(text: string): string {
    const div = document.createElement("div");
    div.textContent = text;
    return div.innerHTML;
  }
}
