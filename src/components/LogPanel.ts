import { logService, LogEntry } from "../services/logService";
import i18n from "../i18n";

export class LogPanel {
  private container: HTMLElement;
  private logArea: HTMLElement;
  private clearBtn: HTMLButtonElement | null;
  private titleEl: HTMLElement | null;

  private handleLanguageChange = () => {
    this.updateStaticLabels();
    const currentLogs = logService.getLogs();
    if (!currentLogs || currentLogs.length === 0) {
      this.update([]);
    }
  };

  constructor(containerId: string) {
    const container = document.getElementById(containerId);
    if (!container) throw new Error(`找不到元素: ${containerId}`);
    this.container = container;

    this.render();

    this.logArea = this.container.querySelector("#log-content")!;
    this.clearBtn = this.container.querySelector("#clear-log");
    this.titleEl = this.container.querySelector("#lp-title");

    this.clearBtn?.addEventListener("click", () => {
      logService.clear();
    });

    i18n.on("languageChanged", this.handleLanguageChange);
    this.updateStaticLabels();

    logService.onUpdate((logs) => this.update(logs));
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
          <button id="clear-log" class="btn-icon" type="button">
            <i class="fa-solid fa-trash-can" aria-hidden="true"></i>
          </button>
        </div>
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
    if (this.clearBtn) {
      this.clearBtn.setAttribute("title", i18n.t("logs.clear"));
      this.clearBtn.setAttribute("aria-label", i18n.t("logs.clear"));
    }
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
