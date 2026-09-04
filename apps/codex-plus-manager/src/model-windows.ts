import { isValidAutoCompactPercent, normalizeAutoCompactPercent } from "./auto-compact.ts";

type ModelWindowsMapParseResult =
  | { ok: true; map: Record<string, string> }
  | { ok: false; error: string };

const INVALID_MODEL_WINDOWS_ERROR = "每模型上下文配置不是有效 JSON 对象；原始值尚未改动，请编辑任意模型或窗口后再保存。";

function parseModelWindowsMap(modelWindows: string, error = INVALID_MODEL_WINDOWS_ERROR): ModelWindowsMapParseResult {
  const serialized = modelWindows.trim();
  if (!serialized) return { ok: true, map: {} };
  try {
    const parsed = JSON.parse(serialized) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return { ok: false, error };
    }
    const map: Record<string, string> = {};
    for (const [model, window] of Object.entries(parsed)) {
      if (typeof window !== "string") {
        return { ok: false, error };
      }
      map[model] = window;
    }
    return { ok: true, map };
  } catch {
    return { ok: false, error };
  }
}

/// 把 model_windows JSON map 按 model_list 行顺序转成文本（每行一个窗口，空行表示默认）。
export function modelWindowsMapToText(modelList: string, modelWindows: string): string {
  const parsed = parseModelWindowsMap(modelWindows);
  if (!parsed.ok) return "";
  return modelList
    .split("\n")
    .map((line) => parsed.map[line.trim()] ?? "")
    .join("\n");
}

/// 把左右 textarea 文本组装成 model_windows JSON map。
export function modelWindowsTextToMap(modelList: string, modelWindowsText: string): string {
  const models = modelList.split("\n").map((s) => s.trim()).filter(Boolean);
  const windows = modelWindowsText.split("\n").map((s) => s.trim());
  const map: Record<string, string> = {};
  models.forEach((model, index) => {
    if (windows[index]) {
      map[model] = windows[index];
    }
  });
  return JSON.stringify(map);
}

/// 图片处理模式。
export type ImageHandling = "" | "send-as-is" | "strip" | "vlm";

export type ModelWindowRow = {
  model: string;
  window: string;
  /// 自动压缩百分比；空值不写覆盖，保持 Codex 默认行为。
  autoCompact: string;
  imageHandling: ImageHandling;
};

export type ModelWindowRowsFromProfileResult = {
  rows: ModelWindowRow[];
  validationError: string | null;
};

export type ModelWindowRowsValidationIssue = {
  code: "duplicateModel" | "invalidWindow" | "invalidAutoCompact";
  model: string;
};

export function isValidModelWindow(value: string): boolean {
  const trimmed = value.trim();
  if (!trimmed) return true;
  const match = trimmed.match(/^(\d+)([KkMm])?$/);
  if (!match) return false;
  const multiplier = match[2]?.toLowerCase() === "m"
    ? 1_000_000n
    : match[2]
      ? 1_000n
      : 1n;
  const tokens = BigInt(match[1]) * multiplier;
  return tokens > 0n && tokens <= 9_223_372_036_854_775_807n;
}

export function modelWindowRowsValidationError(rows: ModelWindowRow[]): ModelWindowRowsValidationIssue | null {
  const seen = new Set<string>();
  for (const row of rows) {
    const model = row.model.trim();
    if (!model) continue;
    if (seen.has(model)) return { code: "duplicateModel", model };
    seen.add(model);
    if (!isValidModelWindow(row.window)) return { code: "invalidWindow", model };
    if (!isValidAutoCompactPercent(row.autoCompact ?? "")) {
      return { code: "invalidAutoCompact", model };
    }
  }
  return null;
}

export function mergeModelWindowRows(
  currentRows: ModelWindowRow[],
  incomingRows: ModelWindowRow[],
): ModelWindowRow[] {
  const rows: ModelWindowRow[] = [];
  const seen = new Set<string>();
  const append = (row: ModelWindowRow) => {
    const model = row.model.trim();
    if (!model || seen.has(model)) return;
    seen.add(model);
    rows.push({
      model,
      window: row.window.trim(),
      autoCompact: normalizeAutoCompactPercent(row.autoCompact ?? ""),
      imageHandling: row.imageHandling ?? "send-as-is",
    });
  };
  currentRows.forEach(append);
  incomingRows.forEach(append);
  return rows.length ? rows : [{ model: "", window: "", autoCompact: "", imageHandling: "send-as-is" }];
}

export function modelWindowRowsFromProfile(
  modelList: string,
  modelWindows: string,
  modelVlm?: string,
  modelAutoCompact?: string,
): ModelWindowRowsFromProfileResult {
  const parsedWindows = parseModelWindowsMap(modelWindows);
  const map = parsedWindows.ok ? parsedWindows.map : {};
  const parsedCompact = parseModelWindowsMap(
    modelAutoCompact ?? "",
    "每模型自动压缩配置不是有效 JSON 对象；原始值尚未改动，请编辑模型行后再保存。",
  );
  const autoCompactMap = parsedCompact.ok ? parsedCompact.map : {};
  // 解析 modelVlm JSON：`{"model": "vlm"/"strip"}`
  let vlmMap: Record<string, ImageHandling> = {};
  try {
    const raw = JSON.parse(modelVlm || "{}") as Record<string, unknown>;
    for (const [model, value] of Object.entries(raw)) {
      if (value === "vlm") {
        vlmMap[model] = "vlm";
      } else if (value === "strip") {
        vlmMap[model] = "strip";
      }
      // 其他值 → 不记录
    }
  } catch {
    vlmMap = {};
  }
  const rows = modelList
    .split("\n")
    .map((model) => model.trim())
    .filter(Boolean)
    .map((model) => ({
      model,
      window: map[model] ?? "",
      autoCompact: normalizeAutoCompactPercent(autoCompactMap[model] ?? ""),
      imageHandling: vlmMap[model] ?? "send-as-is",
    }));
  return {
    rows: rows.length ? rows : [{ model: "", window: "", autoCompact: "", imageHandling: "send-as-is" }],
    validationError: !parsedWindows.ok ? parsedWindows.error : !parsedCompact.ok ? parsedCompact.error : null,
  };
}

export function serializeModelWindowRows(rows: ModelWindowRow[]): {
  modelList: string;
  modelWindows: string;
  modelVlm: string;
  modelAutoCompact: string;
} {
  const modelList: string[] = [];
  const modelWindows: Record<string, string> = {};
  const modelVlm: Record<string, string> = {};
  const modelAutoCompact: Record<string, string> = {};
  mergeModelWindowRows(rows, []).forEach((row) => {
    const model = row.model.trim();
    if (!model) return;
    modelList.push(model);
    const window = row.window.trim();
    if (window) {
      modelWindows[model] = window;
    }
    // 只持久化非默认值
    if (row.imageHandling === "vlm" || row.imageHandling === "strip") {
      modelVlm[model] = row.imageHandling;
    }
    const autoCompact = normalizeAutoCompactPercent(row.autoCompact?.trim() ?? "");
    if (autoCompact) modelAutoCompact[model] = autoCompact;
  });
  return {
    modelList: modelList.join("\n"),
    modelWindows: JSON.stringify(modelWindows),
    modelVlm: JSON.stringify(modelVlm),
    modelAutoCompact: JSON.stringify(modelAutoCompact),
  };
}

export type BuildModelWindowsResult =
  | { ok: true; modelWindows: string }
  | { ok: false; error: string };

/// 校验模型列表与窗口文本行数一致，并组装成 model_windows JSON。
export function buildModelWindows(modelList: string, modelWindowsText: string): BuildModelWindowsResult {
  const models = modelList.split("\n").map((s) => s.trim()).filter(Boolean);
  const windows = modelWindowsText.split("\n").map((s) => s.trim());
  if (models.length !== windows.length) {
    return {
      ok: false,
      error: `模型名称有 ${models.length} 行，上下文窗口有 ${windows.length} 行，请保持行数一致。`,
    };
  }
  return { ok: true, modelWindows: modelWindowsTextToMap(modelList, modelWindowsText) };
}
