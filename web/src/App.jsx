import React, { useState, useEffect } from 'react';
import Sidebar from './components/Sidebar';
import Header from './components/Header';
import ProvidersPage from './pages/ProvidersPage';
import ApiKeysPage from './pages/ApiKeysPage';
import ModelsCatalogPage from './pages/ModelsCatalogPage';
import UsagePage from './pages/UsagePage';
import SettingsPage from './pages/SettingsPage';
import { api } from './api/client';

export default function App() {
  const [currentTab, setCurrentTab] = useState('providers');
  const [isHealthy, setIsHealthy] = useState(true);
  const [refreshCounter, setRefreshCounter] = useState(0);
  const [isRefreshing, setIsRefreshing] = useState(false);

  // Check health periodically
  useEffect(() => {
    let isMounted = true;
    const check = async () => {
      const res = await api.getHealth();
      if (isMounted) {
        setIsHealthy(res.ok !== false);
      }
    };
    check();
    const interval = setInterval(check, 15000);
    return () => {
      isMounted = false;
      clearInterval(interval);
    };
  }, []);

  const handleRefresh = async () => {
    setIsRefreshing(true);
    setRefreshCounter((c) => c + 1);
    const health = await api.getHealth();
    setIsHealthy(health.ok !== false);
    setTimeout(() => setIsRefreshing(false), 500);
  };

  const PAGE_TITLES = {
    providers: {
      title: 'Provider Connections',
      subtitle: 'Upstream OpenAI, Anthropic, Gemini, Vertex, and custom AI nodes',
    },
    keys: {
      title: 'Client API Keys',
      subtitle: 'Manage client credentials, rate limits, and access controls',
    },
    models: {
      title: 'Model Catalog',
      subtitle: 'OpenAI-compatible /v1/models endpoint directory',
    },
    usage: {
      title: 'Usage & Analytics',
      subtitle: 'Throughput metrics, token counting, and system latency',
    },
    settings: {
      title: 'System Settings',
      subtitle: '9Router Rust Core runtime and proxy configuration',
    },
  };

  return (
    <div className="flex h-screen bg-slate-950 text-slate-100 overflow-hidden font-sans">
      {/* Sidebar */}
      <Sidebar
        currentTab={currentTab}
        onSelectTab={setCurrentTab}
        isHealthy={isHealthy}
      />

      {/* Main Content Area */}
      <div className="flex-1 flex flex-col min-w-0 overflow-hidden">
        <Header
          title={PAGE_TITLES[currentTab]?.title || '9Router'}
          subtitle={PAGE_TITLES[currentTab]?.subtitle || ''}
          onRefresh={handleRefresh}
          isRefreshing={isRefreshing}
        />

        <main className="flex-1 overflow-y-auto">
          {currentTab === 'providers' && <ProvidersPage onRefreshTrigger={refreshCounter} />}
          {currentTab === 'keys' && <ApiKeysPage onRefreshTrigger={refreshCounter} />}
          {currentTab === 'models' && <ModelsCatalogPage onRefreshTrigger={refreshCounter} />}
          {currentTab === 'usage' && <UsagePage onRefreshTrigger={refreshCounter} />}
          {currentTab === 'settings' && <SettingsPage onRefreshTrigger={refreshCounter} />}
        </main>
      </div>
    </div>
  );
}
