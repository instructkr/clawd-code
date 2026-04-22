import { useState } from "react";
import LeftPanel from "./components/panels/LeftPanel";
import CenterPanel from "./components/panels/CenterPanel";
import RightPanel from "./components/panels/RightPanel";
import SettingsModal from "./components/modals/SettingsModal";


function App() {
  const [isSettingsOpen, setIsSettingsOpen] = useState(false);

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-base text-text selection:bg-surface2">
      {/* Left Panel */}
      <LeftPanel />

      {/* Center Panel (Resizable/Flexible) */}
      <CenterPanel onOpenSettings={() => setIsSettingsOpen(true)} />

      {/* Right Panel */}
      <RightPanel />

      {/* Modals */}
      {isSettingsOpen && <SettingsModal onClose={() => setIsSettingsOpen(false)} />}
    </div>
  );
}

export default App;
