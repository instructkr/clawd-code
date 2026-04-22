import { useState, useEffect } from "react";
import { X, RefreshCw } from "lucide-react";
import { useSettings } from "../../hooks/useSettings";

export default function SettingsModal({ onClose }: { onClose: () => void }) {
  const [activeTab, setActiveTab] = useState("main-chat-model");
  const { settings, saveSettings } = useSettings();

  // Local state for editing before saving
  const [localSettings, setLocalSettings] = useState(settings);
  const [ollamaModels, setOllamaModels] = useState<string[]>([]);
  const [isLoadingModels, setIsLoadingModels] = useState(false);

  const handleSave = () => {
    saveSettings(localSettings);
    onClose();
  };

  const fetchOllamaModels = async () => {
    setIsLoadingModels(true);
    try {
      const url = localSettings.baseUrl || "http://localhost:11434";
      const response = await fetch(`${url}/api/tags`);
      const data = await response.json();
      if (data.models && Array.isArray(data.models)) {
        setOllamaModels(data.models.map((m: any) => m.name));
      }
    } catch (e) {
      console.error("Failed to fetch Ollama models:", e);
    } finally {
      setIsLoadingModels(false);
    }
  };

  useEffect(() => {
    if (localSettings.provider === 'Ollama (Local)' && activeTab === 'main-chat-model') {
      fetchOllamaModels();
    }
  }, [localSettings.provider, activeTab]);

  return (
    <div className="fixed inset-0 bg-crust/80 backdrop-blur-sm z-50 flex items-center justify-center p-4">
      <div className="bg-base border border-surface1 rounded-xl shadow-2xl w-full max-w-2xl flex flex-col max-h-[80vh]">

        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-surface0">
          <h2 className="text-lg font-semibold text-text">Settings</h2>
          <button
            onClick={onClose}
            className="p-1 text-subtext0 hover:text-text hover:bg-surface0 rounded-md transition-colors"
          >
            <X size={20} />
          </button>
        </div>

        {/* Content */}
        <div className="flex flex-1 min-h-0">
          {/* Sidebar */}
          <div className="w-48 border-r border-surface0 p-2 flex flex-col gap-1 overflow-y-auto bg-mantle/30">
            {["General", "Main Chat Model", "Embeddings", "API Keys"].map((tab) => {
              const id = tab.toLowerCase().replace(/ /g, "-");
              return (
                <button
                  key={id}
                  onClick={() => setActiveTab(id)}
                  className={`text-left px-3 py-2 text-sm rounded-md transition-colors ${
                    activeTab === id
                      ? "bg-surface0 text-text font-medium"
                      : "text-subtext0 hover:text-text hover:bg-surface0/50"
                  }`}
                >
                  {tab}
                </button>
              );
            })}
          </div>

          {/* Settings Area */}
          <div className="flex-1 p-6 overflow-y-auto">
            {activeTab === "main-chat-model" && (
              <div className="space-y-6">
                <div>
                  <h3 className="text-sm font-medium text-text mb-3">Model Provider</h3>
                  <select
                    value={localSettings.provider}
                    onChange={(e) => setLocalSettings({...localSettings, provider: e.target.value})}
                    className="w-full bg-mantle border border-surface1 rounded-md px-3 py-2 text-sm focus:outline-none focus:border-blue text-text"
                  >
                    <option>Anthropic</option>
                    <option>OpenAI / OpenRouter</option>
                    <option>Ollama (Local)</option>
                    <option>xAI</option>
                  </select>
                </div>

                <div>
                  <h3 className="text-sm font-medium text-text mb-3">Model Name</h3>
                  {localSettings.provider === 'Ollama (Local)' ? (
                    <div className="flex gap-2">
                      <select
                        value={localSettings.modelName}
                        onChange={(e) => setLocalSettings({...localSettings, modelName: e.target.value})}
                        className="flex-1 bg-mantle border border-surface1 rounded-md px-3 py-2 text-sm focus:outline-none focus:border-blue text-text"
                      >
                        {ollamaModels.length === 0 && <option value={localSettings.modelName}>{localSettings.modelName}</option>}
                        {ollamaModels.map(model => (
                          <option key={model} value={model}>{model}</option>
                        ))}
                      </select>
                      <button
                        onClick={fetchOllamaModels}
                        className="p-2 bg-surface0 hover:bg-surface1 rounded-md border border-surface1 transition-colors flex items-center justify-center"
                        title="Refresh models"
                      >
                        <RefreshCw size={18} className={`text-subtext0 ${isLoadingModels ? 'animate-spin' : ''}`} />
                      </button>
                    </div>
                  ) : (
                    <input
                      type="text"
                      placeholder="e.g. claude-3-5-sonnet"
                      value={localSettings.modelName}
                      onChange={(e) => setLocalSettings({...localSettings, modelName: e.target.value})}
                      className="w-full bg-mantle border border-surface1 rounded-md px-3 py-2 text-sm focus:outline-none focus:border-blue text-text"
                    />
                  )}
                  <p className="text-xs text-subtext0 mt-2">
                    For Ollama, select the provider above to scan local models.
                  </p>
                </div>

                <div>
                  <h3 className="text-sm font-medium text-text mb-3">Base URL (Optional)</h3>
                  <input
                    type="text"
                    placeholder="http://localhost:11434"
                    value={localSettings.baseUrl}
                    onChange={(e) => setLocalSettings({...localSettings, baseUrl: e.target.value})}
                    className="w-full bg-mantle border border-surface1 rounded-md px-3 py-2 text-sm focus:outline-none focus:border-blue text-text"
                  />
                </div>
              </div>
            )}

            {activeTab === "api-keys" && (
              <div className="space-y-6">
                <div>
                  <h3 className="text-sm font-medium text-text mb-3">Anthropic API Key</h3>
                  <input
                    type="password"
                    placeholder="sk-ant-..."
                    value={localSettings.anthropicKey}
                    onChange={(e) => setLocalSettings({...localSettings, anthropicKey: e.target.value})}
                    className="w-full bg-mantle border border-surface1 rounded-md px-3 py-2 text-sm focus:outline-none focus:border-blue text-text"
                  />
                </div>
                <div>
                  <h3 className="text-sm font-medium text-text mb-3">OpenAI / Compatible API Key</h3>
                  <input
                    type="password"
                    placeholder="sk-..."
                    value={localSettings.openAiKey}
                    onChange={(e) => setLocalSettings({...localSettings, openAiKey: e.target.value})}
                    className="w-full bg-mantle border border-surface1 rounded-md px-3 py-2 text-sm focus:outline-none focus:border-blue text-text"
                  />
                </div>
              </div>
            )}

            {activeTab === "general" && (
              <div className="space-y-6">
                <div>
                  <h3 className="text-sm font-medium text-text mb-3">Language</h3>
                  <select
                    value={localSettings.language}
                    onChange={(e) => setLocalSettings({...localSettings, language: e.target.value as 'en' | 'tr'})}
                    className="w-full bg-mantle border border-surface1 rounded-md px-3 py-2 text-sm focus:outline-none focus:border-blue text-text"
                  >
                    <option value="en">English</option>
                    <option value="tr">Turkish (Türkçe)</option>
                  </select>
                </div>
                <div>
                  <h3 className="text-sm font-medium text-text mb-3">Theme</h3>
                  <select
                    value={localSettings.theme}
                    onChange={(e) => setLocalSettings({...localSettings, theme: e.target.value as 'dark' | 'light'})}
                    className="w-full bg-mantle border border-surface1 rounded-md px-3 py-2 text-sm focus:outline-none focus:border-blue text-text"
                  >
                    <option value="dark">Dark (Catppuccin)</option>
                    <option value="light">Light</option>
                  </select>
                </div>
              </div>
            )}

            {activeTab === "embeddings" && (
              <div className="text-sm text-subtext0">
                Workspace embedding model settings will appear here.
              </div>
            )}
          </div>
        </div>

        {/* Footer */}
        <div className="p-4 border-t border-surface0 bg-mantle/50 flex justify-end gap-3">
          <button
            onClick={onClose}
            className="px-4 py-2 text-sm font-medium text-text hover:bg-surface0 rounded-md transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            className="px-4 py-2 text-sm font-medium bg-blue text-crust hover:bg-blue/90 rounded-md transition-colors"
          >
            Save Changes
          </button>
        </div>
      </div>
    </div>
  );
}
