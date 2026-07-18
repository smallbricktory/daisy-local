// In-call chat panel: a persistent, multi-turn conversation scoped to ONE
// session. During a live recording it feeds the model the transcript lines
// added since the last reply; on a finished session it just continues the
// saved thread. Cross-meeting questions are out of scope — see the footer.
import { useEffect, useRef, useState } from 'react';
import { tauri, errStr, type CallChatMsg } from '../tauri';
import { getLiveTranscriptState } from '../liveTranscript';
import { transcriptTailSince } from '../lib/transcriptTail';
import { showGatewayNoticeIfNeeded } from '../lib/gatewayNotice';
import { MarkdownView } from './MarkdownView';

export function CallChat({ sessionId, live, onThreadChange }: {
  sessionId: string;
  live: boolean;
  /** Reports whether a persisted thread exists (load + after each saved reply). */
  onThreadChange?: (hasMessages: boolean) => void;
}) {
  const [messages, setMessages] = useState<CallChatMsg[]>([]);
  const [cursor, setCursor] = useState(0);
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    tauri.liveChatLoad(sessionId)
      .then((c) => { if (!cancelled) { setMessages(c.messages); setCursor(c.transcript_cursor_ms); onThreadChange?.(c.messages.length > 0); } })
      .catch(() => { /* empty thread is fine */ });
    return () => { cancelled = true; };
  }, [sessionId]);

  useEffect(() => { endRef.current?.scrollIntoView({ behavior: 'smooth' }); }, [messages, sending]);

  async function send() {
    const text = input.trim();
    if (!text || sending) return;
    setError(null);
    setSending(true);
    // Optimistically show the user's turn while the model thinks.
    setMessages((m) => [...m, { role: 'user', content: text }]);
    setInput('');
    // Streams the reply token-by-token into an assistant bubble appended on
    // the first delta. `started` lives in this closure (not React state);
    // the onToken callbacks share it synchronously.
    let started = false;
    try {
      const tail = live
        ? transcriptTailSince(getLiveTranscriptState().finals, cursor)
        : { text: '', endMs: cursor };
      const res = await tauri.liveChatSendStream(
        {
          session_id: sessionId,
          user_text: text,
          transcript_tail: tail.text,
          tail_end_ms: tail.endMs,
        },
        (delta) => {
          setMessages((m) => {
            const next = [...m];
            if (!started) {
              next.push({ role: 'assistant', content: delta });
              started = true;
            } else {
              const last = next[next.length - 1];
              next[next.length - 1] = { ...last, content: last.content + delta };
            }
            return next;
          });
        },
      );
      // Replace the optimistic stream with the authoritative persisted thread.
      setMessages(res.chat.messages);
      setCursor(res.chat.transcript_cursor_ms);
      onThreadChange?.(res.chat.messages.length > 0);
    } catch (e) {
      // Roll back the partial assistant bubble (if any) + the optimistic user bubble.
      setMessages((m) => {
        const n = started ? m.slice(0, -1) : m;
        return n.slice(0, -1);
      });
      setInput(text);
      if (!showGatewayNoticeIfNeeded(e)) setError(errStr(e));
    } finally {
      setSending(false);
    }
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 8, maxHeight: 320, overflowY: 'auto', padding: '4px 2px' }}>
        {messages.length === 0 && (
          <p className="meta" style={{ fontSize: 13 }}>
            Ask about this meeting{live ? ' as it happens' : ''} — e.g. “what did we decide?”,
            “summarize the last few minutes”, “what action items came up?”.
          </p>
        )}
        {messages.map((m, i) => (
          <div
            key={i}
            style={{
              alignSelf: m.role === 'user' ? 'flex-end' : 'flex-start',
              maxWidth: '85%',
              background: m.role === 'user' ? 'var(--indigo-deep, #3b3b6d)' : 'var(--frost, #f0f0f4)',
              color: m.role === 'user' ? '#fff' : 'var(--ink, #111)',
              borderRadius: 10,
              padding: '6px 10px',
              fontSize: 14,
            }}
          >
            {m.role === 'assistant'
              ? <MarkdownView markdown={m.content} />
              : <span>{m.content}</span>}
          </div>
        ))}
        {sending && messages[messages.length - 1]?.role !== 'assistant' && (
          <p className="meta" style={{ fontSize: 12, alignSelf: 'flex-start' }}>Thinking…</p>
        )}
        <div ref={endRef} />
      </div>

      {error && <p className="meta" style={{ color: 'var(--danger, #c0392b)', fontSize: 12 }}>{error}</p>}

      <div style={{ display: 'flex', gap: 8 }}>
        <input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); void send(); } }}
          placeholder="Ask about this meeting…"
          disabled={sending}
          style={{ flex: 1 }}
        />
        <button className="btn btn--primary" onClick={() => void send()} disabled={sending || !input.trim()}>
          Send
        </button>
      </div>

      <p className="meta" style={{ fontSize: 11, lineHeight: 1.4 }}>
        This chat only sees <strong>this</strong> meeting. Looking across past meetings? Use <strong>Search</strong>.
      </p>
    </div>
  );
}
