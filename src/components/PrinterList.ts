import { printerApi, Printer } from "../services/printerService";
import i18n from "../i18n";

export class PrinterList {
  private container: HTMLElement;
  private listContainer: HTMLElement;
  private refreshBtn: HTMLButtonElement;
  private printers: Printer[] = [];
  private sharedPrinterIds: Set<string> = new Set();
  private isSwitchingTarget = false;
  private loadRequestId = 0;
  private refreshTimer: ReturnType<typeof setInterval>;

  private handleLanguageChange = () => {
    this.renderStaticLabels();
    this.renderList();
  };

  constructor(containerId: string) {
    const container = document.getElementById(containerId);
    if (!container) throw new Error(`找不到元素: ${containerId}`);
    this.container = container;

    this.render();
    this.listContainer = this.container.querySelector("#printer-items")!;
    this.refreshBtn = this.container.querySelector("#refresh-btn")!;

    this.bindEvents();

    i18n.on("languageChanged", this.handleLanguageChange);

    this.load();
    this.refreshTimer = setInterval(() => this.load(), 30_000);
  }

  public destroy() {
    i18n.off("languageChanged", this.handleLanguageChange);
    clearInterval(this.refreshTimer);
  }

  private render() {
    this.container.innerHTML = `
      <section class="panel">
        <div class="panel-header">
          <div class="panel-title-group">
            <span class="panel-icon"><i class="fa-solid fa-print"></i></span>
            <h2 id="pl-title" class="panel-title"></h2>
          </div>
          <div class="flex items-center gap-2">
            <button id="enable-lan-btn" class="btn-apple" type="button">
              <i class="fa-solid fa-shield-halved"></i>
              <span id="enable-lan-text"></span>
            </button>
            <button id="refresh-btn" class="btn-icon" type="button">
              <i class="fa-solid fa-rotate" id="refresh-icon"></i>
            </button>
          </div>
        </div>
        <p id="network-scope-note" class="hint" style="margin-bottom: 8px;"></p>
        <p id="target-note" class="hint" style="margin-bottom: 8px;"></p>
        <div id="printer-items">
          <div class="hint" id="pl-loading-text"></div>
        </div>
      </section>
    `;

    this.renderStaticLabels();
  }

  private renderStaticLabels() {
    const titleEl = this.container.querySelector("#pl-title");
    const loadingTextEl = this.container.querySelector("#pl-loading-text");
    const enableLanEl = this.container.querySelector("#enable-lan-text");
    const scopeNoteEl = this.container.querySelector("#network-scope-note");
    const targetNoteEl = this.container.querySelector("#target-note");
    const refreshBtn = this.container.querySelector("#refresh-btn");

    if (titleEl) titleEl.textContent = i18n.t("printers.title");
    if (loadingTextEl) loadingTextEl.textContent = i18n.t("common.loading");
    if (enableLanEl) enableLanEl.textContent = i18n.t("actions.enable_lan_access");
    if (scopeNoteEl) scopeNoteEl.textContent = i18n.t("network.lan_access_scope");
    if (targetNoteEl) targetNoteEl.textContent = i18n.t("printers.target_hint");
    if (refreshBtn) refreshBtn.setAttribute("title", i18n.t("actions.refresh"));
  }

  private bindEvents() {
    this.refreshBtn.addEventListener("click", () => this.load());
    this.container
      .querySelector<HTMLButtonElement>("#enable-lan-btn")
      ?.addEventListener("click", async (event) => {
        const button = event.currentTarget as HTMLButtonElement;
        button.disabled = true;
        try {
          await printerApi.enableLanAccess();
          alert(i18n.t("messages.lan_access_enabled"));
        } catch (error) {
          alert(i18n.t("errors.lan_access_failed", { error: String(error) }));
        } finally {
          button.disabled = false;
        }
      });
  }

  async load() {
    if (this.isSwitchingTarget) return;
    const requestId = ++this.loadRequestId;
    this.setLoading(true);
    const loadingTextEl = this.container.querySelector("#pl-loading-text");
    if (loadingTextEl) loadingTextEl.textContent = i18n.t("common.loading");

    try {
      const [printers, shared] = await Promise.all([
        printerApi.getList(),
        printerApi.getSharedList(),
      ]);

      if (requestId !== this.loadRequestId || this.isSwitchingTarget) return;

      this.printers = printers;
      this.sharedPrinterIds = new Set(shared.map((p) => p.id));
      this.renderList();
    } catch (error) {
      if (requestId !== this.loadRequestId || this.isSwitchingTarget) return;
      const errorMsg = i18n.t("errors.load_failed", { error: String(error) });
      this.listContainer.innerHTML = `<div class="hint" style="color:#ff3b30;">${escapeHtml(errorMsg)}</div>`;
    } finally {
      this.setLoading(false);
    }
  }

  private renderList() {
    if (this.printers.length === 0) {
      this.listContainer.innerHTML = `<div class="hint" style="padding: 8px 12px;">${i18n.t("printers.no_printers")}</div>`;
      return;
    }

    this.listContainer.innerHTML = this.printers
      .map((p) => {
        const statusStr = (p.status || "").toString().toLowerCase();
        const isOnline = statusStr === "online" || statusStr === "busy";
        const isShared = this.sharedPrinterIds.has(p.id);

        const statusKey =
          statusStr === "busy" ? "status.busy" : isOnline ? "status.online" : "status.offline";
        const statusText = i18n.t(statusKey);
        const dotClass = isOnline ? "status-dot-online" : "status-dot-offline";
        const disabled = this.isSwitchingTarget || (!isOnline && !isShared);

        return `
        <div class="printer-row">
          <span class="printer-icon"><i class="fa-solid fa-print"></i></span>
          <div class="printer-meta">
            <div class="printer-name">${escapeHtml(p.name)}</div>
            <div class="printer-status">
              <span class="status-dot ${dotClass}"></span>
              <span>${statusText}</span>
            </div>
            ${isShared ? `<div class="printer-status">${escapeHtml(i18n.t("printers.selected_target", { name: p.name }))}</div>` : ""}
          </div>
          <input
            type="radio"
            name="airprinter-target"
            class="radio radio-primary share-toggle"
            aria-label="${escapeHtml(i18n.t("printers.select_target", { name: p.name }))}"
            data-id="${escapeHtml(p.id)}"
            ${isShared ? "checked" : ""}
            ${disabled ? "disabled" : ""}
          />
          ${isShared ? `<button class="btn-apple stop-target-btn" data-id="${escapeHtml(p.id)}" type="button" ${this.isSwitchingTarget ? "disabled" : ""}>${escapeHtml(i18n.t("actions.stop_sharing"))}</button>` : ""}
        </div>
      `;
      })
      .join("");

    this.listContainer.querySelectorAll<HTMLInputElement>(".share-toggle").forEach((toggle) => {
      toggle.addEventListener("change", () => {
        const id = toggle.dataset.id!;
        if (toggle.checked) this.handleShare(id);
      });
    });

    this.listContainer.querySelectorAll<HTMLButtonElement>(".stop-target-btn").forEach((button) => {
      button.addEventListener("click", () => this.handleStop(button.dataset.id!));
    });
  }

  private async handleShare(printerId: string) {
    if (this.isSwitchingTarget) return;
    const previousIds = new Set(this.sharedPrinterIds);
    if (previousIds.size === 1 && previousIds.has(printerId)) return;
    this.isSwitchingTarget = true;
    this.loadRequestId++;
    this.setLoading(true);
    this.renderList();

    try {
      if (!previousIds.has(printerId)) await printerApi.share(printerId);
      this.sharedPrinterIds = new Set([printerId]);
    } catch (error) {
      this.sharedPrinterIds = previousIds;
      const errorMsg = i18n.t("errors.operation_failed", { error: String(error) });
      alert(`❌ ${errorMsg}`);
    } finally {
      this.isSwitchingTarget = false;
      this.setLoading(false);
      this.renderList();
    }
  }

  private async handleStop(printerId: string) {
    if (this.isSwitchingTarget) return;
    const previousIds = new Set(this.sharedPrinterIds);
    this.isSwitchingTarget = true;
    this.loadRequestId++;
    this.setLoading(true);
    this.renderList();
    try {
      await printerApi.unshare(printerId);
      this.sharedPrinterIds = new Set();
    } catch (error) {
      this.sharedPrinterIds = previousIds;
      const errorMsg = i18n.t("errors.operation_failed", { error: String(error) });
      alert(`❌ ${errorMsg}`);
    } finally {
      this.isSwitchingTarget = false;
      this.setLoading(false);
      this.renderList();
    }
  }

  private setLoading(loading: boolean) {
    const icon = this.container.querySelector("#refresh-icon");
    if (icon) icon.classList.toggle("fa-spin", loading);
    this.refreshBtn.disabled = loading || this.isSwitchingTarget;

    if (loading && this.printers.length === 0) {
      const loadingTextEl = this.container.querySelector("#pl-loading-text");
      if (loadingTextEl) loadingTextEl.textContent = i18n.t("common.loading");
    }
  }
}

function escapeHtml(value: string): string {
  const span = document.createElement("span");
  span.textContent = value;
  return span.innerHTML.replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}
