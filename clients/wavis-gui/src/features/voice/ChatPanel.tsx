import { memo, useRef, useState, type ReactNode } from 'react';
import { useAutoScrollAnchor } from '@shared/hooks/useAutoScrollAnchor';
import { openExternalUrl } from '@shared/shell-bridge';
import {
  buildChatDisplayItems,
  formatTime,
  resolveChatMessageDisplayColor,
  type ChatMessage,
} from './chat-display-model';
import { chatLinkTarget } from './chat-links';
import { sendChatMessage } from './voice-room';

const CHAT_LINK_RE = /(https?:\/\/[^\s<]+|www\.[^\s<]+)/gi;
const TRAILING_LINK_PUNCTUATION_RE = /[.,!?;:)\]]+$/;

function normalizeChatLink(raw: string): string {
  return raw.toLowerCase().startsWith('www.') ? `https://${raw}` : raw;
}

function splitChatLink(raw: string): { hrefText: string; trailingText: string } {
  const trailingText = raw.match(TRAILING_LINK_PUNCTUATION_RE)?.[0] ?? '';
  return trailingText
    ? { hrefText: raw.slice(0, -trailingText.length), trailingText }
    : { hrefText: raw, trailingText: '' };
}

function renderChatText(text: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let lastIndex = 0;

  for (const match of text.matchAll(CHAT_LINK_RE)) {
    const raw = match[0];
    const index = match.index;
    if (index > lastIndex) nodes.push(text.slice(lastIndex, index));

    const { hrefText, trailingText } = splitChatLink(raw);
    const href = normalizeChatLink(hrefText);
    nodes.push(
      <a
        key={`link-${index}-${hrefText}`}
        href={href}
        target={chatLinkTarget(href)}
        className="text-wavis-accent underline underline-offset-2 break-words hover:opacity-80"
        onClick={(event) => {
          event.preventDefault();
          void openExternalUrl(href);
        }}
        rel="noreferrer"
        title={href}
      >
        {hrefText}
      </a>,
    );
    if (trailingText) nodes.push(trailingText);
    lastIndex = index + raw.length;
  }

  if (lastIndex < text.length) nodes.push(text.slice(lastIndex));
  return nodes;
}

export interface ChatPanelProps {
  chatMessages: ChatMessage[];
  participants: Array<{ id: string; userId?: string; color: string }>;
  chatError: string | null;
}

function areChatPanelPropsEqual(prev: ChatPanelProps, next: ChatPanelProps): boolean {
  if (prev.chatError !== next.chatError) return false;
  if (prev.chatMessages.length !== next.chatMessages.length) return false;
  if (prev.chatMessages.length > 0) {
    const lastIdx = prev.chatMessages.length - 1;
    if (prev.chatMessages[lastIdx].id !== next.chatMessages[lastIdx].id) return false;
  }
  return true;
}

function ChatPanelImpl({ chatMessages, participants, chatError }: ChatPanelProps) {
  const [chatInput, setChatInput] = useState('');
  const chatThrottledRef = useRef(false);
  const chatEndRef = useAutoScrollAnchor<HTMLDivElement>(chatMessages.length);

  const handleSendChat = () => {
    if (chatThrottledRef.current) return;
    const text = chatInput.trim();
    if (!text) return;
    setChatInput('');
    sendChatMessage(text);
    chatThrottledRef.current = true;
    setTimeout(() => {
      chatThrottledRef.current = false;
    }, 200);
  };

  return (
    <div className="flex-1 flex flex-col min-h-0">
      <div className="px-3 py-3 border-b border-wavis-text-secondary h-[4.5rem] flex flex-col justify-center">
        <div className="font-bold text-sm">CHAT</div>
      </div>
      <div className="flex-1 overflow-y-auto overflow-x-hidden p-4 space-y-1 text-sm">
        {chatMessages.length === 0 && (
          <div className="text-wavis-text-secondary">No messages yet</div>
        )}
        {buildChatDisplayItems(chatMessages).map((item) =>
          item.type === 'date-divider' ? (
            <div key={item.id} className="text-wavis-text-secondary text-xs py-1 text-center">
              {'─'.repeat(12)} {item.label} {'─'.repeat(12)}
            </div>
          ) : (
            <div key={item.message.id} className="break-words">
              <span className="text-wavis-text-secondary">
                [{formatTime(item.message.timestamp)}]
              </span>{' '}
              <span
                style={{
                  color: resolveChatMessageDisplayColor(item.message, participants),
                }}
              >
                {item.message.displayName}
              </span>
              <span>: {renderChatText(item.message.text)}</span>
            </div>
          ),
        )}
        {chatError && <div className="text-wavis-text-secondary italic text-xs">{chatError}</div>}
        <div ref={chatEndRef} />
      </div>
      <div className="p-4 border-t border-wavis-text-secondary">
        <div className="flex items-center gap-2">
          <span className="text-wavis-accent">&gt;</span>
          <input
            type="text"
            value={chatInput}
            onChange={(e) => setChatInput(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSendChat()}
            maxLength={2000}
            className="flex-1 bg-transparent border-b border-wavis-text-secondary outline-none px-2 py-1 font-mono text-wavis-text"
            placeholder="type message..."
          />
        </div>
      </div>
    </div>
  );
}

export const ChatPanel = memo(ChatPanelImpl, areChatPanelPropsEqual);
