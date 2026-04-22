import { useState } from "react";
import { Settings, Paperclip, Send, Terminal, Cpu, Loader2 } from "lucide-react";
import Markdown from "react-markdown";
// @ts-ignore
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
// @ts-ignore
import { vscDarkPlus } from "react-syntax-highlighter/dist/esm/styles/prism";
import { useClawEngine } from "../../hooks/useClawEngine";
import { useSettings } from "../../hooks/useSettings";

export default function CenterPanel({ onOpenSettings }: { onOpenSettings: () => void }) {
  const [input, setInput] = useState("");
  const [isThinkingOpen, setIsThinkingOpen] = useState(false);

  const { settings } = useSettings();
  const { messages, sendMessage, isProcessing, tokenUsage } = useClawEngine();

  const handleSend = () => {
    if (!input.trim() || isProcessing) return;
    sendMessage(input);
    setInput("");
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  return (
    <div className="flex-1 flex flex-col bg-base min-w-0">
      {/* Header */}
      <header className="h-14 border-b border-surface0 flex items-center justify-between px-4 bg-mantle/50">
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-1.5 px-2 py-1 bg-surface0 rounded-md text-xs font-medium text-text border border-surface1">
            <Cpu size={14} className="text-mauve" />
            {settings.modelName || "Select Model"}
          </div>
          <div className="text-xs text-subtext0 flex items-center gap-1">
            <Terminal size={14} />
            <span>Ready</span>
          </div>
        </div>
        <div className="flex items-center gap-4">
          <div className="text-xs text-subtext0 font-mono">
            <span className="text-text">{tokenUsage.toLocaleString()}</span> tk
          </div>
          <button
            onClick={onOpenSettings}
            className="p-1.5 text-subtext0 hover:text-text hover:bg-surface0 rounded transition-colors"
          >
            <Settings size={18} />
          </button>
        </div>
      </header>

      {/* Chat Area */}
      <div className="flex-1 overflow-y-auto p-4 space-y-6">
        {messages.map((msg, i) => (
          <div key={i} className={`flex flex-col max-w-4xl mx-auto ${msg.role === "user" ? "items-end" : "items-start"}`}>
            {msg.thinking && (
              <div className="mb-2 w-full max-w-[85%] border border-surface1 rounded-md overflow-hidden bg-mantle/50 text-sm">
                <button
                  onClick={() => setIsThinkingOpen(!isThinkingOpen)}
                  className="w-full text-left px-3 py-1.5 bg-surface0/30 text-subtext1 flex items-center justify-between hover:bg-surface0/50 transition-colors"
                >
                  <span className="italic flex items-center gap-2">Thinking Process</span>
                  <span>{isThinkingOpen ? "▼" : "▶"}</span>
                </button>
                {isThinkingOpen && (
                  <div className="p-3 text-subtext0 border-t border-surface0 italic">
                    {msg.thinking}
                  </div>
                )}
              </div>
            )}

            <div className={`p-4 rounded-xl max-w-[85%] prose prose-invert max-w-none prose-p:leading-relaxed prose-pre:p-0 ${
              msg.role === "user"
                ? "bg-surface0 text-text rounded-tr-sm"
                : "bg-transparent text-text w-full"
            }`}>
              <Markdown
                components={{
                  code(props) {
                    const {children, className, node, ...rest} = props
                    const match = /language-(\w+)/.exec(className || '')
                    return match ? (
                      <div className="rounded-md overflow-hidden my-3 border border-surface1">
                         <div className="bg-mantle px-3 py-1 text-xs text-subtext0 border-b border-surface0 flex justify-between">
                            {match[1]}
                         </div>
                        <SyntaxHighlighter
                          PreTag="div"
                          children={String(children).replace(/\n$/, '')}
                          language={match[1]}
                          style={vscDarkPlus as any}
                          customStyle={{ margin: 0, padding: '1rem', background: 'var(--color-crust)' }}
                          {...(rest as any)}
                        />
                      </div>
                    ) : (
                      <code {...rest} className="bg-surface1/50 text-pink px-1.5 py-0.5 rounded text-sm font-mono">
                        {children}
                      </code>
                    )
                  }
                }}
              >
                {msg.content}
              </Markdown>
            </div>
          </div>
        ))}
      </div>

      {/* Input Area */}
      <div className="p-4 bg-base border-t border-surface0">
        <div className="max-w-4xl mx-auto relative flex items-end gap-2 bg-mantle border border-surface1 rounded-xl p-2 focus-within:border-mauve/50 transition-colors shadow-sm">
          <button className="p-2 text-subtext0 hover:text-text rounded-lg hover:bg-surface0 transition-colors mb-0.5">
            <Paperclip size={20} />
          </button>
          <textarea
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Ask Claw to build something..."
            className="flex-1 max-h-64 min-h-[44px] bg-transparent border-none resize-none focus:outline-none p-2 text-sm leading-relaxed"
            rows={1}
          />
          <button
            onClick={handleSend}
            disabled={isProcessing || !input.trim()}
            className="p-2 bg-blue hover:bg-blue/90 disabled:opacity-50 disabled:hover:bg-blue text-crust rounded-lg transition-colors mb-0.5 font-medium flex items-center justify-center"
          >
            {isProcessing ? <Loader2 size={18} className="animate-spin" /> : <Send size={18} />}
          </button>
        </div>
      </div>
    </div>
  );
}
