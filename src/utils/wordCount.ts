export interface TextStats {
  /** 字数：中日韩字符逐字计 1，连续英文/数字串计 1，不含标点与空白（同 Word「字数」口径） */
  words: number;
  /** 字符数：去空白后的字符总数（含标点） */
  chars: number;
}

/** 中英混排字数统计。只统计 text 本身，不要传入 segments 的 chars（时间轴 JSON） */
export function countText(text: string): TextStats {
  let words = 0;
  let chars = 0;
  let inWord = false;
  for (const ch of text) {
    if (/\s/.test(ch)) {
      inWord = false;
      continue;
    }
    chars++;
    if (/[\u4e00-\u9fff\u3040-\u30ff\uac00-\ud7af]/.test(ch)) {
      words++;
      inWord = false;
    } else if (/[A-Za-z0-9]/.test(ch)) {
      if (!inWord) {
        words++;
        inWord = true;
      }
    } else {
      inWord = false;
    }
  }
  return { words, chars };
}
