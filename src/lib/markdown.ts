import DOMPurify from "dompurify";
import { marked, type Renderer } from "marked";
import { createHighlighter, type Highlighter } from "shiki";

const LANGS = [
  "typescript",
  "javascript",
  "python",
  "bash",
  "shell",
  "json",
  "html",
  "css",
  "rust",
  "text",
] as const;
const THEMES = ["github-light", "github-dark"] as const;

let _highlighter: Highlighter | null = null;
let _highlighterPromise: Promise<Highlighter> | null = null;

async function getHighlighter(): Promise<Highlighter> {
  if (_highlighter) return _highlighter;
  if (!_highlighterPromise) {
    _highlighterPromise = createHighlighter({ themes: [...THEMES], langs: [...LANGS] }).then(
      (h) => {
        _highlighter = h;
        return h;
      }
    );
  }
  return _highlighterPromise;
}

/** Render markdown to sanitized HTML with Shiki-highlighted code blocks. */
export async function renderMarkdown(md: string): Promise<string> {
  const highlighter = await getHighlighter();
  const loadedLangs = highlighter.getLoadedLanguages();

  const renderer: Partial<Renderer> = {
    code({ text, lang }) {
      const language = lang && loadedLangs.includes(lang as never) ? lang : "text";
      return highlighter.codeToHtml(text, {
        lang: language,
        themes: { light: "github-light", dark: "github-dark" },
      });
    },
  };

  marked.use({ renderer });
  const raw = await marked(md);
  // DOMPurify keeps inline `style` by default, which Shiki needs for colors.
  return DOMPurify.sanitize(raw);
}
