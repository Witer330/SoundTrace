import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  changeDataRoot,
  errMsg,
  getAppInfo,
  getSettings,
  setSettings,
} from "../api";
import type { AppInfo } from "../types";

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

/** 设置页：存储 / 转写模型 / AI 复盘 / 关于 */
export default function SettingsView() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [llmBaseUrl, setLlmBaseUrl] = useState("");
  const [llmApiKey, setLlmApiKey] = useState("");
  const [llmModel, setLlmModel] = useState("");
  const [error, setError] = useState("");
  const [savedFlash, setSavedFlash] = useState(false);
  const [rootFlash, setRootFlash] = useState("");

  useEffect(() => {
    Promise.all([getAppInfo(), getSettings()])
      .then(([i, s]) => {
        setInfo(i);
        setLlmBaseUrl(s["llm.baseUrl"] ?? "");
        setLlmApiKey(s["llm.apiKey"] ?? "");
        setLlmModel(s["llm.model"] ?? "");
      })
      .catch((e) => setError(errMsg(e)));
  }, []);

  async function saveLlm() {
    try {
      await setSettings({
        "llm.baseUrl": llmBaseUrl.trim(),
        "llm.apiKey": llmApiKey.trim(),
        "llm.model": llmModel.trim(),
      });
      setSavedFlash(true);
      setTimeout(() => setSavedFlash(false), 1500);
    } catch (e) {
      setError(errMsg(e));
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
          <div className="px-3 py-2 rounded-lg bg-red-900/40 border border-red-800/60 text-red-200 text-sm">
            {error}
          </div>
        )}

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

        <Card
          title="转写模型（本地离线）"
          desc="Paraformer-large（字符级时间戳）+ silero-VAD + CT-Transformer 标点"
        >
          <div className="text-xs text-mist-400">
            模型下载与管理功能将在 M3 里程碑提供，届时首次转写前在此下载（约 260MB）。
          </div>
        </Card>

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
          <div className="flex items-center gap-3">
            <button
              className="px-3 py-1.5 rounded-lg text-sm bg-ink-700 hover:bg-ink-600 text-mist-200"
              onClick={saveLlm}
            >
              保存
            </button>
            {savedFlash && <span className="text-xs text-jade-accent">已保存 ✓</span>}
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
