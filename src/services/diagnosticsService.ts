import { invoke } from "@tauri-apps/api/core";

/** Location and size of the on-disk diagnostics log. */
export interface LogInfo {
  path: string;
  size_bytes: number;
}

export async function getLogInfo(): Promise<LogInfo> {
  return invoke<LogInfo>("get_log_info");
}

/** Newest persisted diagnostics content (bounded by the backend). */
export async function readLogs(): Promise<string> {
  return invoke<string>("read_logs");
}

/** Reveal the diagnostics log file in Explorer/Finder. */
export async function openLogFolder(): Promise<void> {
  return invoke("open_log_folder");
}
