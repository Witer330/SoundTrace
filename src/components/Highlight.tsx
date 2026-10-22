import { Fragment } from "react";

/** 把文本按搜索词切分为普通/命中片段（大小写不敏感的子串匹配） */
export function splitHighlight(text: string, query: string): { t: string; hit: boolean }[] {
  if (!query) return [{ t: text, hit: false }];
  const out: { t: string; hit: boolean }[] = [];
  const lower = text.toLowerCase();
  const q = query.toLowerCase();
  let i = 0;
  while (i < text.length) {
    const idx = lower.indexOf(q, i);
    if (idx === -1) {
      out.push({ t: text.slice(i), hit: false });
      break;
    }
    if (idx > i) out.push({ t: text.slice(i, idx), hit: false });
    out.push({ t: text.slice(idx, idx + q.length), hit: true });
    i = idx + q.length;
  }
  return out;
}

/** 搜索词高亮：query 为空时原样输出 */
export default function Highlight({ text, query }: { text: string; query: string }) {
  const q = query.trim();
  if (!q) return <>{text}</>;
  return (
    <>
      {splitHighlight(text, q).map(({ t, hit }, i) =>
        hit ? (
          <mark key={i} className="rounded bg-cyan-accent/30 px-px text-mist-200">
            {t}
          </mark>
        ) : (
          <Fragment key={i}>{t}</Fragment>
        ),
      )}
    </>
  );
}
