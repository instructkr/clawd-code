import { useState, useEffect } from 'react';

export interface AppSettings {
  language: 'en' | 'tr';
  theme: 'dark' | 'light';
  provider: string;
  modelName: string;
  baseUrl: string;
  anthropicKey: string;
  openAiKey: string;
}

const defaultSettings: AppSettings = {
  language: 'en',
  theme: 'dark',
  provider: 'Anthropic',
  modelName: 'claude-3-5-sonnet',
  baseUrl: '',
  anthropicKey: '',
  openAiKey: ''
};

export function useSettings() {
  const [settings, setSettings] = useState<AppSettings>(() => {
    const saved = localStorage.getItem('appSettings');
    if (saved) {
      try {
        return { ...defaultSettings, ...JSON.parse(saved) };
      } catch (e) {
        return defaultSettings;
      }
    }
    return defaultSettings;
  });

  const saveSettings = (newSettings: Partial<AppSettings>) => {
    const updated = { ...settings, ...newSettings };
    setSettings(updated);
    localStorage.setItem('appSettings', JSON.stringify(updated));
  };

  useEffect(() => {
    // Apply theme on load
    if (settings.theme === 'dark') {
      document.documentElement.classList.add('dark');
    } else {
      document.documentElement.classList.remove('dark');
    }
  }, [settings.theme]);

  return { settings, saveSettings };
}
