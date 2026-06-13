import { useState } from 'preact/hooks';
import { ThemeProvider } from './theme/ThemeContext';
import { StoreProvider, useStore } from './lib/store';
import { ToastProvider } from './components/Toast';
import { TabBar } from './components/TabBar';
import { NowPlayingTab } from './tabs/NowPlaying';
import { DevicesTab } from './tabs/Devices';
import { EqTab } from './tabs/EQ';
import { StatusTab } from './tabs/Status';
import './styles/app.css';

export type TabId = 'now-playing' | 'devices' | 'eq' | 'status';

function AppInner() {
  const [activeTab, setActiveTab] = useState<TabId>('now-playing');
  const store = useStore();

  return (
    <div class="app-shell">
      <div class="app-shell__bg" aria-hidden="true" />
      <TabBar active={activeTab} onSelect={setActiveTab} wsConnected={store.wsConnected} />

      <main class="app-shell__content">
        <div
          id="panel-now-playing"
          role="tabpanel"
          aria-labelledby="tab-now-playing"
          hidden={activeTab !== 'now-playing'}
        >
          <NowPlayingTab />
        </div>
        <div
          id="panel-devices"
          role="tabpanel"
          aria-labelledby="tab-devices"
          hidden={activeTab !== 'devices'}
        >
          <DevicesTab />
        </div>
        <div
          id="panel-eq"
          role="tabpanel"
          aria-labelledby="tab-eq"
          hidden={activeTab !== 'eq'}
        >
          <EqTab />
        </div>
        <div
          id="panel-status"
          role="tabpanel"
          aria-labelledby="tab-status"
          hidden={activeTab !== 'status'}
        >
          <StatusTab />
        </div>
      </main>
    </div>
  );
}

export function App() {
  return (
    <ThemeProvider>
      <StoreProvider>
        <ToastProvider>
          <AppInner />
        </ToastProvider>
      </StoreProvider>
    </ThemeProvider>
  );
}
