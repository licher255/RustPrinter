// src/services/printerService.ts
import { invoke } from "@tauri-apps/api/core"; 
import { logService } from "./logService";
import i18n from "../i18n"; // 确保引入

export interface MediaSize {
  name: string;
  /** IPP dimensions, in hundredths of a millimetre. */
  width: number;
  height: number;
  windows_kind: number;
}

export interface PrinterCapabilities {
  color: boolean;
  duplex: boolean;
  max_copies: number;
  media: string[];
  default_media: string;
  media_sizes: MediaSize[];
}

export interface Printer {
  name: string;
  id: string;
  status: "online" | "offline" | "busy" | string;
  capabilities?: PrinterCapabilities;
  /** Supply/hardware warnings decoded from Windows status bits, e.g. "toner_low". */
  alerts?: string[];
}

export const printerApi = {
  async enableLanAccess(): Promise<void> {
    await invoke("enable_lan_access");
  },
  async getList(): Promise<Printer[]> {
    logService.add(i18n.t('logs.fetching_printers'), "info");
    try {
        const printers = await invoke<Printer[]>("get_printers");
        
        // ✅ 修改这里：翻译 console.log
        console.log(i18n.t('debug.printers_received', { count: printers.length, data: JSON.stringify(printers) }));
        // 或者简单点：
        // console.log(`[DEBUG] ${i18n.t('logs.found_printers', { count: printers.length })}`, printers);
        
        logService.add(i18n.t('logs.found_printers', { count: printers.length }), "success");
        return printers;
    } catch (error) {
        logService.add(i18n.t('errors.fetch_failed', { error: String(error) }), "error");
        throw error;
    }
  },

  async share(printerId: string): Promise<string> {
    // ✅ 修改这里
    console.log(i18n.t('debug.sharing_request', { id: printerId }));
    
    logService.add(i18n.t('logs.sharing_printer', { id: printerId }), "info");
    try {
      const result = await invoke<string>("share_printer", { printerId });
      logService.add(result, "success");
      return result;
    } catch (error) {
      logService.add(i18n.t('errors.share_failed', { error: String(error) }), "error");
      throw error;
    }
  },

  async stop(printerId: string): Promise<void> {
    await invoke("stop_printer", { printerId });
    logService.add(i18n.t('logs.stopped_sharing', { id: printerId }), "info");
  },

  async getSharedList(): Promise<Printer[]> {
      return await invoke<Printer[]>("get_shared_printers");
  },

  async unshare(printerId: string): Promise<void> {
      await invoke("unshare_printer", { printerId });
      logService.add(i18n.t('logs.stopped_sharing', { id: printerId }), "info");
  },

};
