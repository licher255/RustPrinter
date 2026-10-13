import { printerApi, Printer, PrinterCapabilities } from "../services/printerService";
import i18n from "../i18n";

/** Friendly labels for the well-known IPP media keywords. */
const MEDIA_LABELS: Record<string, string> = {
  iso_a4_210x297mm: "A4",
  iso_a5_148x210mm: "A5",
  "na_letter_8.5x11in": "Letter",
  "na_legal_8.5x14in": "Legal",
};

function mediaLabel(
  name: string,
  sizes: { name: string; width: number; height: number }[]
): string {
  if (MEDIA_LABELS[name]) return MEDIA_LABELS[name];
  const size = sizes.find((s) => s.name === name);
  if (size) return `${size.width / 100}×${size.height / 100}mm`;
  return name;
}

export class PrinterList {
  private container: HTMLElement;
  private listContainer: HTMLElement;
  private refreshBtn: HTMLButtonElement;
  private printers: Printer[] = [];
  private sharedPrinterIds: Set<string> = new Set();
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
              <i class="fa-solid fa-shield-halved" aria-hidden="true"></i>
              <span id="enable-lan-text"></span>
            </button>
            <button id="refresh-btn" class="btn-icon" type="button">
              <i class="fa-solid fa-rotate" id="refresh-icon" aria-hidden="true"></i>
            </button>
          </div>
        </div>
        <p id="network-scope-note" class="hint" style="margin-bottom: 8px;"></p>
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
    const refreshBtn = this.container.querySelector("#refresh-btn");

    if (titleEl) titleEl.textContent = i18n.t("printers.title");
    if (loadingTextEl) loadingTextEl.textContent = i18n.t("common.loading");
    if (enableLanEl) enableLanEl.textContent = i18n.t("actions.enable_lan_access");
    if (scopeNoteEl) scopeNoteEl.textContent = i18n.t("network.lan_access_scope");
    if (refreshBtn) {
      refreshBtn.setAttribute("title", i18n.t("actions.refresh"));
      refreshBtn.setAttribute("aria-label", i18n.t("actions.refresh"));
    }
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
    this.setLoading(true);
    const loadingTextEl = this.container.querySelector("#pl-loading-text");
    if (loadingTextEl) loadingTextEl.textContent = i18n.t("common.loading");

    try {
      const [printers, shared] = await Promise.all([
        printerApi.getList(),
        printerApi.getSharedList(),
      ]);

      this.printers = printers;
      this.sharedPrinterIds = new Set(shared.map((p) => p.id));
      this.renderList();
    } catch (error) {
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
        const disabled = !isOnline && !isShared;

        return `
        <div class="printer-row">
          <span class="printer-icon"><i class="fa-solid fa-print"></i></span>
          <div class="printer-meta">
            <div class="printer-name">${escapeHtml(p.name)}</div>
            <div class="printer-status">
              <span class="status-dot ${dotClass}"></span>
              <span>${statusText}</span>
            </div>
            ${this.renderCaps(p.capabilities)}
            ${this.renderAlerts(p.alerts)}
          </div>
          <input
            type="checkbox"
            class="toggle toggle-primary share-toggle"
            data-id="${escapeHtml(p.id)}"
            aria-label="${escapeHtml(`${i18n.t(isShared ? "actions.stop_sharing" : "actions.share")}: ${p.name}`)}"
            ${isShared ? "checked" : ""}
            ${disabled ? "disabled" : ""}
          />
        </div>
      `;
      })
      .join("");

    this.listContainer.querySelectorAll<HTMLInputElement>(".share-toggle").forEach((toggle) => {
      toggle.addEventListener("change", () => {
        const id = toggle.dataset.id!;
        this.handleShare(id, toggle);
      });
    });
  }

  private renderCaps(caps?: PrinterCapabilities): string {
    if (!caps) return "";
    const badge = (
      key: string,
      icon: string,
      supported: boolean,
      params?: Record<string, unknown>
    ) =>
      `<span class="cap-badge ${supported ? "cap-yes" : "cap-no"}"><i class="fa-solid ${icon}" aria-hidden="true"></i>${escapeHtml(i18n.t(key, params))}</span>`;

    const badges: string[] = [
      badge(caps.color ? "caps.color" : "caps.mono", "fa-palette", caps.color),
      badge(caps.duplex ? "caps.duplex_auto" : "caps.duplex_none", "fa-copy", caps.duplex),
    ];
    if (caps.max_copies > 1) {
      badges.push(badge("caps.copies", "fa-layer-group", true, { count: caps.max_copies }));
    }
    let mediaDetail = "";
    if (caps.media.length > 0) {
      const all = caps.media.map((m) => mediaLabel(m, caps.media_sizes));
      const defaultLabel = mediaLabel(caps.default_media, caps.media_sizes);
      const summary =
        all.length > 1
          ? i18n.t("caps.media_more", { name: defaultLabel, count: all.length })
          : defaultLabel;
      badges.push(
        `<span class="cap-badge cap-yes cap-media" title="${escapeHtml(all.join(", "))}"><i class="fa-solid fa-sheet-plastic" aria-hidden="true"></i>${escapeHtml(i18n.t("caps.media", { list: summary }))}</span>`
      );
      const formatMm = (hundredths: number) =>
        (hundredths / 100).toFixed(1).replace(/\.0$/, "");
      const details = caps.media.map((m) => {
        const size = caps.media_sizes.find((s) => s.name === m);
        const dims = size ? ` ${formatMm(size.width)}×${formatMm(size.height)}mm` : "";
        const text = `${mediaLabel(m, caps.media_sizes)}${dims}`;
        return m === caps.default_media
          ? `${text}${i18n.t("caps.default_mark")}`
          : text;
      });
      mediaDetail = `<div class="printer-media-detail">${escapeHtml(
        i18n.t("caps.media_detail", { count: all.length, list: details.join("、") })
      )}</div>`;
    }
    return `<div class="printer-caps">${badges.join("")}</div>${mediaDetail}`;
  }

  private renderAlerts(alerts?: string[]): string {
    if (!alerts || alerts.length === 0) return "";
    const badges = alerts.map((alert) => {
      const label = i18n.t(`alerts.${alert}`, { defaultValue: alert });
      return `<span class="cap-badge cap-alert"><i class="fa-solid fa-triangle-exclamation" aria-hidden="true"></i>${escapeHtml(label)}</span>`;
    });
    return `<div class="printer-caps">${badges.join("")}</div>`;
  }

  private async handleShare(printerId: string, toggle: HTMLInputElement) {
    const wantShare = toggle.checked;
    toggle.disabled = true;

    try {
      if (wantShare) {
        await printerApi.share(printerId);
        this.sharedPrinterIds.add(printerId);
      } else {
        await printerApi.unshare(printerId);
        this.sharedPrinterIds.delete(printerId);
      }
    } catch (error) {
      toggle.checked = !wantShare;
      const errorMsg = i18n.t("errors.operation_failed", { error: String(error) });
      alert(`❌ ${errorMsg}`);
    } finally {
      toggle.disabled = false;
    }
  }

  private setLoading(loading: boolean) {
    const icon = this.container.querySelector("#refresh-icon");
    if (icon) icon.classList.toggle("fa-spin", loading);
    this.refreshBtn.disabled = loading;

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
