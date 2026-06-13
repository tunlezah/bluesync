import type { TabId } from '../app';
import './TabBar.css';

interface Tab {
  id: TabId;
  label: string;
  icon: string;
}

const TABS: Tab[] = [
  { id: 'now-playing', label: 'Now Playing', icon: '♪' },
  { id: 'devices',     label: 'Devices',     icon: '⊕' },
  { id: 'eq',          label: 'EQ',          icon: '≈' },
  { id: 'status',      label: 'Status',      icon: '⚙' },
];

interface Props {
  active: TabId;
  onSelect: (id: TabId) => void;
  wsConnected: boolean;
}

export function TabBar({ active, onSelect, wsConnected }: Props) {
  return (
    <nav class="tab-bar" role="tablist" aria-label="App sections">
      {TABS.map(tab => (
        <button
          key={tab.id}
          class={`tab-bar__item${active === tab.id ? ' tab-bar__item--active' : ''}`}
          role="tab"
          aria-selected={active === tab.id}
          aria-controls={`panel-${tab.id}`}
          onClick={() => onSelect(tab.id)}
        >
          <span class="tab-bar__icon" aria-hidden="true">{tab.icon}</span>
          <span class="tab-bar__label">{tab.label}</span>
        </button>
      ))}
      {/* WS connection indicator */}
      <div
        class={`tab-bar__ws-dot status-dot ${wsConnected ? 'status-dot--active' : 'status-dot--connecting'}`}
        title={wsConnected ? 'Connected' : 'Reconnecting…'}
        aria-label={wsConnected ? 'WebSocket connected' : 'WebSocket reconnecting'}
      />
    </nav>
  );
}
