import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import {
  changeDataRoot,
  downloadModels,
  errMsg,
  getAppInfo,
  getSettings,
  modelsStatus,
  setSettings,
  testLlm,
  type ModelDownloadProgress,
  type ModelStatus,
} from "../api";
import type { AppInfo } from "../types";
import { formatBytes } from "../types";
import { useAppStore, type ThemeMode } from "../store";

function Card({
  title,
  desc,
  children,
}: {
  title: string;
  desc?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-xl border border-ink-800 bg-ink-900 p-4 space-y-3">
      <div>
        <div className="text-mist-300 text-sm font-medium">{title}</div>
        {desc && <div className="text-xs text-ink-500 mt-0.5">{desc}</div>}
      </div>
      {children}
    </section>
  );
}

/** 外观：深色 / 浅色 / 跟随系统 */
function AppearanceCard() {
  const themeMode = useAppStore((s) => s.themeMode);
  const setThemeMode = useAppStore((s) => s.setThemeMode);

  const options: { key: ThemeMode; label: string; hint: string }[] = [
    { key: "system", label: "跟随系统", hint: "随 Windows 的深浅色设置自动切换" },
    { key: "dark", label: "深色", hint: "深空蓝底，长时间盯波形不刺眼" },
    { key: "light", label: "浅色", hint: "近白底，白天与投屏时更清晰" },
  ];

  return (
    <Card title="外观" desc="主题切换即时生效，选择会记住并在下次启动时直接套用">
      <div className="flex gap-2">
        {options.map((o) => (
          <button
            key={o.key}
            onClick={() => setThemeMode(o.key)}
            title={o.hint}
            className={`flex-1 px-3 py-2 rounded-lg text-sm border transition-colors ${
              themeMode === o.key
                ? "bg-ink-700 border-cyan-accent/60 text-mist-100 font-medium"
                : "bg-ink-850 border-ink-700 text-mist-300 hover:border-ink-600"
            }`}
          >
            {o.label}
          </button>
        ))}
      </div>
      <div className="text-[11px] text-ink-500">{options.find((o) => o.key === themeMode)?.hint}</div>
    </Card>
  );
}

/** 模型管理卡片 */
function ModelsCard() {
  const [statuses, setStatuses] = useState<ModelStatus[]>([]);
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress] = useState<Record<string, ModelDownloadProgress>>({});
  const [error, setError] = useState("");

  function refresh() {
    modelsStatus().then(setStatuses).catch((e) => setError(errMsg(e)));
  }

  useEffect(() => {
    refresh();
    const p = listen<ModelDownloadProgress>("models://progress", (e) => {
      setProgress((prev) => ({ ...prev, [e.payload.modelId]: e.payload }));
    });
    const d = listen<{ ok: boolean; error: string }>("models://done", (e) => {
      setDownloading(false);
      setProgress({});
      if (!e.payload.ok) setError(e.payload.error || "下载失败");
      refresh();
    });
    return () => {
      void p.then((fn) => fn());
      void d.then((fn) => fn());
    };
  }, []);

  const missing = statuses.filter((m) => !m.ready).map((m) => m.id);
  const totalMissingBytes = statuses
    .filter((m) => !m.ready)
    .reduce((acc, m) => acc + (m.totalBytes - m.presentBytes), 0);

  return (
    <Card
      title="离线模型（本地推理）"
      desc="Paraformer-large（字符级时间戳）+ silero-VAD + CT-Transformer 标点 + pyannote / CAM++（说话人分离）· 全部在本机推理"
    >
      {error && <div className="text-xs text-st-err">{error}</div>}
      <div className="space-y-2">
        {statuses.map((m) => {
          const p = progress[m.id];
          return (
            <div key={m.id} className="flex items-center gap-3 text-xs">
              <span
                className={`w-2 h-2 rounded-full shrink-0 ${
                  m.ready ? "bg-jade-accent" : p ? "bg-cyan-accent animate-pulse" : "bg-ink-600"
                }`}
              />
              <span className="flex-1 text-mist-300">{m.name}</span>
              {p ? (
                <span className="text-ink-500 tabular-nums">
                  {formatBytes(p.bytes)} / {p.total ? formatBytes(p.total) : "?"} · {p.file}
                </span>
              ) : m.ready ? (
                <span className="text-jade-accent">就绪</span>
              ) : (
                <span className="text-ink-500">约 {formatBytes(m.totalBytes)}</span>
              )}
            </div>
          );
        })}
      </div>
      {statuses.length > 0 && missing.length > 0 && (
        <button
          className="px-3 py-1.5 rounded-lg text-sm bg-gradient-to-r from-teal-accent to-cyan-accent text-ink-950 font-medium hover:opacity-90 disabled:opacity-50"
          onClick={() => {
            setError("");
            setDownloading(true);
            downloadModels(missing).catch((e) => {
              setDownloading(false);
              setError(errMsg(e));
            });
          }}
          disabled={downloading}
        >
          {downloading
            ? `下载中（约 ${formatBytes(totalMissingBytes)}）…`
            : `下载缺失模型（约 ${formatBytes(totalMissingBytes)}）`}
        </button>
      )}
      {downloading && (
        <div className="w-full h-2 rounded-full bg-ink-800 overflow-hidden">
          <div
            className="h-full bg-gradient-to-r from-teal-accent to-cyan-accent transition-all"
            style={{
              width: `${Math.min(
                100,
                Math.round(
                  (Object.values(progress).reduce((a, p) => a + p.bytes, 0) /
                    Math.max(1, totalMissingBytes)) *
                    100,
                ),
              )}%`,
            }}
          />
        </div>
      )}
    </Card>
  );
}

/** 转写性能：限制转写能吃多少 CPU。
 *
 * 转写是本机纯 CPU 计算。默认「自动」= 物理核心数（至少留 2 个逻辑核心），并且转写进程
 * 始终以低于前台的优先级运行：机器空闲时满速，一旦你动手就立刻让路。
 * 注意别随手选「全部核心」——开满逻辑核心在 HT / 大小核机器上实测反而慢一倍（后端有实测表）。
 */
function PerfCard({
  logicalCores,
  autoThreads,
}: {
  logicalCores: number | null;
  autoThreads: number | null;
}) {
  const [mode, setMode] = useState<"auto" | "all" | "custom">("auto");
  const [custom, setCustom] = useState<number | null>(null);
  const [savedFlash, setSavedFlash] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    getSettings()
      .then((s) => {
        const raw = (s["perf.asrThreads"] ?? "").trim();
        if (raw === "all") {
          setMode("all");
          return;
        }
        const n = Number(raw);
        if (raw !== "" && Number.isInteger(n) && n >= 1) {
          setMode("custom");
          setCustom(n);
        }
      })
      .catch((e) => setError(errMsg(e)));
  }, []);

  const logical = logicalCores ?? 0;
  const auto = autoThreads ?? 0;
  const customValue = custom ?? (auto > 0 ? auto : 4);
  // 与后端 transcribe::parse_threads 一致：自定义值收敛到 [1, logical]
  const clampToLogical = (n: number) =>
    logical > 0 ? Math.min(Math.max(1, n), logical) : Math.max(1, n);

  const options: { key: "auto" | "all" | "custom"; label: string }[] = [
    { key: "auto", label: "自动（推荐）" },
    { key: "all", label: "全部核心" },
    { key: "custom", label: "自定义" },
  ];

  const core = (n: number) => (logical > 0 ? `${n} / ${logical}` : `${n}`);
  const hint =
    mode === "auto"
      ? `转写用 ${core(auto)} 个逻辑核心（物理核心数，至少留 2 个给系统）`
      : mode === "all"
        ? `转写用全部${logical > 0 ? ` ${logical} 个` : ""}逻辑核心——HT / 大小核机器上开满实测反而更慢，一般不必选`
        : `转写用 ${core(clampToLogical(customValue))} 个逻辑核心`;

  async function save() {
    const value =
      mode === "auto" ? "auto" : mode === "all" ? "all" : String(clampToLogical(customValue));
    try {
      await setSettings({ "perf.asrThreads": value });
      setSavedFlash(true);
      setTimeout(() => setSavedFlash(false), 1500);
    } catch (e) {
      setError(errMsg(e));
    }
  }

  return (
    <Card
      title="转写性能"
      desc="转写进程始终以低于前台的优先级运行：机器空闲时满速，你一动手就让路。这里进一步限制它占用的核心数。"
    >
      {error && <div className="text-xs text-st-err">{error}</div>}
      <div className="flex gap-2">
        {options.map((o) => (
          <button
            key={o.key}
            onClick={() => setMode(o.key)}
            className={`flex-1 px-3 py-2 rounded-lg text-sm border transition-colors ${
              mode === o.key
                ? "bg-ink-700 border-cyan-accent/60 text-mist-100 font-medium"
                : "bg-ink-850 border-ink-700 text-mist-300 hover:border-ink-600"
            }`}
          >
            {o.label}
          </button>
        ))}
      </div>
      {mode === "custom" && (
        <label className="flex items-center gap-2 text-xs text-ink-500">
          线程数
          <input
            type="number"
            className="w-20"
            min={1}
            max={logical > 0 ? logical : 64}
            value={customValue}
            onChange={(e) => setCustom(Math.max(1, Number(e.target.value) || 1))}
          />
          {logical > 0 && <span>本机共 {logical} 个逻辑核心</span>}
        </label>
      )}
      <div className="text-[11px] text-ink-500">{hint}</div>
      <div className="flex items-center gap-3">
        <button
          className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200"
          onClick={save}
        >
          保存
        </button>
        {savedFlash && <span className="text-xs text-jade-accent">已保存 ✓</span>}
      </div>
      <div className="text-[11px] text-ink-500">
        改动只影响之后排队或重新转写的任务，正在转写的那条不受影响
      </div>
    </Card>
  );
}

/** 设置页：存储 / 转写模型 / AI 复盘 / 关于 */
export default function SettingsView() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [llmBaseUrl, setLlmBaseUrl] = useState("");
  const [llmApiKey, setLlmApiKey] = useState("");
  const [llmModel, setLlmModel] = useState("");
  const [llmThinking, setLlmThinking] = useState<"off" | "on">("off");
  const [error, setError] = useState("");
  const [savedFlash, setSavedFlash] = useState(false);
  const [rootFlash, setRootFlash] = useState("");
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState("");

  useEffect(() => {
    Promise.all([getAppInfo(), getSettings()])
      .then(([i, s]) => {
        setInfo(i);
        setLlmBaseUrl(s["llm.baseUrl"] ?? "");
        setLlmApiKey(s["llm.apiKey"] ?? "");
        setLlmModel(s["llm.model"] ?? "");
        setLlmThinking(s["llm.thinking"] === "on" ? "on" : "off");
      })
      .catch((e) => setError(errMsg(e)));
  }, []);

  async function saveLlm() {
    try {
      await setSettings({
        "llm.baseUrl": llmBaseUrl.trim(),
        "llm.apiKey": llmApiKey.trim(),
        "llm.model": llmModel.trim(),
        "llm.thinking": llmThinking,
      });
      setSavedFlash(true);
      setTimeout(() => setSavedFlash(false), 1500);
    } catch (e) {
      setError(errMsg(e));
    }
  }

  async function runTestLlm() {
    setTesting(true);
    setTestResult("");
    try {
      await setSettings({
        "llm.baseUrl": llmBaseUrl.trim(),
        "llm.apiKey": llmApiKey.trim(),
        "llm.model": llmModel.trim(),
        "llm.thinking": llmThinking,
      });
      const reply = await testLlm();
      setTestResult(`连接成功：${reply.slice(0, 50)}`);
    } catch (e) {
      setTestResult(`失败：${errMsg(e)}`);
    } finally {
      setTesting(false);
    }
  }

  async function pickRoot() {
    const dir = await open({ directory: true, multiple: false });
    if (!dir) return;
    try {
      await changeDataRoot(dir);
      setRootFlash("已记录，重启应用后生效（旧目录数据请手动拷贝到新目录）");
    } catch (e) {
      setError(errMsg(e));
    }
  }

  return (
    <div className="h-full overflow-y-auto p-4">
      <div className="max-w-2xl space-y-4">
        {error && (
          <div className="px-3 py-2 rounded-lg bg-st-err-soft border border-st-err/40 text-st-err text-sm">
            {error}
          </div>
        )}

        <AppearanceCard />

        <Card title="存储" desc="录音归档、数据库与模型文件的位置">
          <div className="flex items-center gap-2">
            <code className="flex-1 px-3 py-2 rounded-lg bg-ink-850 border border-ink-700 text-xs text-mist-300 truncate">
              {info?.dataRoot ?? "…"}
            </code>
            <button
              className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200 shrink-0"
              onClick={pickRoot}
            >
              修改…
            </button>
          </div>
          {rootFlash && <div className="text-xs text-jade-accent">{rootFlash}</div>}
          <div className="text-[11px] text-ink-500">
            目录结构：library/（按年月归档音频）· models/（转写模型）· cache/（中间缓存，可随时清空）
          </div>
        </Card>

        <ModelsCard />

        <PerfCard
          logicalCores={info?.logicalCores ?? null}
          autoThreads={info?.autoThreads ?? null}
        />

        <Card
          title="AI 复盘（LLM）"
          desc="OpenAI 兼容接口（GLM / DeepSeek / OpenAI 等）。仅转写文本会发送到所配置的服务，录音永不外传。"
        >
          <label className="block space-y-1">
            <span className="text-xs text-ink-500">Base URL</span>
            <input
              type="text"
              className="w-full"
              value={llmBaseUrl}
              onChange={(e) => setLlmBaseUrl(e.target.value)}
              placeholder="https://open.bigmodel.cn/api/paas/v4"
            />
          </label>
          <label className="block space-y-1">
            <span className="text-xs text-ink-500">API Key</span>
            <input
              type="password"
              className="w-full"
              value={llmApiKey}
              onChange={(e) => setLlmApiKey(e.target.value)}
              placeholder="sk-…"
            />
          </label>
          <label className="block space-y-1">
            <span className="text-xs text-ink-500">模型</span>
            <input
              type="text"
              className="w-full"
              value={llmModel}
              onChange={(e) => setLlmModel(e.target.value)}
              placeholder="glm-4.7"
            />
          </label>
          <div className="space-y-1">
            <span className="text-xs text-ink-500">深度思考</span>
            <div className="flex gap-2">
              {(
                [
                  ["off", "关闭（推荐）"],
                  ["on", "开启"],
                ] as const
              ).map(([v, label]) => (
                <button
                  key={v}
                  onClick={() => setLlmThinking(v)}
                  className={`flex-1 px-3 py-2 rounded-lg text-sm border transition-colors ${
                    llmThinking === v
                      ? "bg-ink-700 border-cyan-accent/60 text-mist-100 font-medium"
                      : "bg-ink-850 border-ink-700 text-mist-300 hover:border-ink-600"
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>
            <div className="text-[11px] text-ink-500">
              关闭：出稿快、省 token，原话引用更忠实，纪要提炼足够用。开启：条目更细但耗时与
              token 成倍增加，且输出上限需相应调大。
            </div>
          </div>
          <div className="flex items-center gap-3">
            <button
              className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200"
              onClick={saveLlm}
            >
              保存
            </button>
            <button
              className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200 disabled:opacity-50"
              onClick={runTestLlm}
              disabled={testing}
            >
              {testing ? "测试中…" : "测试连接"}
            </button>
            {savedFlash && <span className="text-xs text-jade-accent">已保存 ✓</span>}
            {testResult && (
              <span
                className={`text-xs truncate max-w-xs ${
                  testResult.startsWith("失败") ? "text-st-err" : "text-jade-accent"
                }`}
              >
                {testResult}
              </span>
            )}
          </div>
        </Card>

        <Card title="关于">
          <div className="text-xs text-mist-400 space-y-1">
            <div>声迹 SoundTrace v{info?.version ?? "…"} · 个人本地工具</div>
            <div>录音与转写在本地完成，数据不出本机（AI 复盘除外，且需显式触发）</div>
          </div>
        </Card>
      </div>
    </div>
  );
}
