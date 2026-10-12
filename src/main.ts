import "@fortawesome/fontawesome-free/css/all.min.css";
import { getVersion } from "@tauri-apps/api/app";
import "./styles.css";
import i18n from "./i18n";
import { PrinterList } from "./components/PrinterList";
import { LogPanel } from "./components/LogPanel";
import { Settings } from "./components/Settings";
import { setupTitlebar } from "./components/Titlebar";

/**
 * 更新页面上所有标记了 data-i18n / data-i18n-title 的元素
 */
const updatePageTranslations = () => {
  document.querySelectorAll<HTMLElement>("[data-i18n]").forEach((el) => {
    const key = el.getAttribute("data-i18n");
    if (key) {
      const translation = i18n.t(key);
      if (translation && translation !== key) {
        el.textContent = translation;
      }
    }
  });
  document.querySelectorAll<HTMLElement>("[data-i18n-title]").forEach((el) => {
    const key = el.getAttribute("data-i18n-title");
    if (key) {
      const translation = i18n.t(key);
      if (translation && translation !== key) {
        el.setAttribute("title", translation);
      }
    }
  });
  document.querySelectorAll<HTMLElement>("[data-i18n-aria-label]").forEach((el) => {
    const key = el.getAttribute("data-i18n-aria-label");
    if (key) {
      const translation = i18n.t(key);
      if (translation && translation !== key) {
        el.setAttribute("aria-label", translation);
      }
    }
  });

  const appTitle = i18n.t("app.title");
  if (appTitle && appTitle !== "app.title") {
    document.title = appTitle.replace(/🖨️\s*/, "");
  }
};

const setupNavigation = () => {
  const tabs = Array.from(document.querySelectorAll<HTMLButtonElement>(".nav-item"));

  const selectTab = (tab: HTMLButtonElement, focusPanel = false) => {
    const view = tab.dataset.view;
    if (!view) return;

    tabs.forEach((item) => {
      const selected = item === tab;
      item.classList.toggle("active", selected);
      item.setAttribute("aria-selected", String(selected));
      item.tabIndex = selected ? 0 : -1;
    });

    document.querySelectorAll<HTMLElement>(".app-view").forEach((panel) => {
      panel.hidden = panel.id !== `view-${view}`;
    });

    if (focusPanel) document.getElementById(`view-${view}`)?.focus();
  };

  tabs.forEach((tab, index) => {
    tab.addEventListener("click", () => selectTab(tab));
    tab.addEventListener("keydown", (event) => {
      let nextIndex = index;
      if (event.key === "ArrowDown" || event.key === "ArrowRight") nextIndex = (index + 1) % tabs.length;
      else if (event.key === "ArrowUp" || event.key === "ArrowLeft") nextIndex = (index - 1 + tabs.length) % tabs.length;
      else if (event.key === "Home") nextIndex = 0;
      else if (event.key === "End") nextIndex = tabs.length - 1;
      else return;

      event.preventDefault();
      selectTab(tabs[nextIndex]);
      tabs[nextIndex].focus();
    });
  });
};

const updateVersionLabel = async () => {
  const label = document.getElementById("app-version");
  if (!label) return;
  let version = "2.0.0";
  if ("__TAURI_INTERNALS__" in window) {
    try {
      version = await getVersion();
    } catch {
      // Keep the build-time fallback visible if the native API is unavailable.
    }
  }
  label.textContent = `${version}.stable`;
};

document.addEventListener("DOMContentLoaded", async () => {
  if (!i18n.isInitialized) {
    await new Promise<void>((resolve) => {
      i18n.on("initialized", () => resolve());
    });
  }

  setupTitlebar();
  setupNavigation();
  await updateVersionLabel();
  new PrinterList("printer-list-container");
  new Settings("settings-container");
  new LogPanel("log-panel-container");
  updatePageTranslations();

  // 全局监听（Settings 组件或浏览器检测切换语言时同步静态文案）
  i18n.on("languageChanged", (lng) => {
    updatePageTranslations();
    document.documentElement.lang = lng;
  });
});
