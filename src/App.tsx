import { useEffect, useMemo, useState } from "react";
import type { AppHealth, FoundationNote } from "./tauriApi";
import { createFoundationNote, getAppHealth, listFoundationNotes } from "./tauriApi";

const navigationItems = ["Bugun", "Takvim", "Musteriler", "Hizmetler", "Personel", "Ayarlar"] as const;

export function App() {
  const [activeItem, setActiveItem] = useState<(typeof navigationItems)[number]>("Bugun");
  const [health, setHealth] = useState<AppHealth | null>(null);
  const [notes, setNotes] = useState<FoundationNote[]>([]);
  const [status, setStatus] = useState("Hazirlaniyor");

  const todayLabel = useMemo(() => {
    return new Intl.DateTimeFormat("tr-TR", {
      weekday: "long",
      day: "numeric",
      month: "long"
    }).format(new Date());
  }, []);

  async function refreshFoundation() {
    const [nextHealth, nextNotes] = await Promise.all([getAppHealth(), listFoundationNotes()]);
    setHealth(nextHealth);
    setNotes(nextNotes);
    setStatus(nextHealth.ok ? "SQLite hazir" : "Kontrol gerekiyor");
  }

  useEffect(() => {
    void refreshFoundation().catch((error) => {
      setStatus(error instanceof Error ? error.message : "Native komut yanit vermedi");
    });
  }, []);

  async function handleFoundationWrite() {
    setStatus("Kaydediliyor");
    await createFoundationNote(`Foundation smoke ${new Date().toISOString()}`);
    await refreshFoundation();
  }

  return (
    <div className="app-shell">
      <aside className="sidebar" aria-label="Ana gezinme">
        <div className="brand">
          <span className="brand-mark">BS</span>
          <div>
            <strong>BeautySaloon</strong>
            <span>Salon gunlugu</span>
          </div>
        </div>
        <nav className="nav-list">
          {navigationItems.map((item) => (
            <button
              key={item}
              type="button"
              className={item === activeItem ? "nav-item is-active" : "nav-item"}
              onClick={() => setActiveItem(item)}
            >
              {item}
            </button>
          ))}
        </nav>
      </aside>

      <main className="content">
        <header className="topbar">
          <div>
            <span className="date-label">{todayLabel}</span>
            <h1>{activeItem}</h1>
          </div>
          <button type="button" className="primary-action">
            + Yeni Randevu
          </button>
        </header>

        <section className="today-panel" aria-labelledby="today-title">
          <div>
            <p className="eyebrow">Foundation</p>
            <h2 id="today-title">Randevu akisi icin temiz baslangic</h2>
            <p>
              Bu shell yeni Tauri mimarisinin ilk iskeletidir. Eski Electron arayuzu kopyalanmadi;
              gunluk salon kullanimi icin sade bir ana akisa yer acildi.
            </p>
          </div>
          <button type="button" className="secondary-action" onClick={() => void handleFoundationWrite()}>
            SQLite yaz/oku testi
          </button>
        </section>

        <section className="status-grid" aria-label="Teknik durum">
          <div className="status-card">
            <span>Native komut</span>
            <strong>{status}</strong>
          </div>
          <div className="status-card">
            <span>Schema</span>
            <strong>{health ? `v${health.schemaVersion}` : "..."}</strong>
          </div>
          <div className="status-card status-card--wide">
            <span>Test veri yolu</span>
            <strong>{health?.databasePath ?? "..."}</strong>
          </div>
        </section>

        <section className="notes-panel" aria-label="SQLite foundation kayitlari">
          <h2>Son SQLite kayitlari</h2>
          {notes.length === 0 ? (
            <p>Henüz smoke kaydi yok.</p>
          ) : (
            <ul>
              {notes.map((note) => (
                <li key={note.id}>
                  <span>{note.title}</span>
                  <time>{new Date(note.createdAtUtc).toLocaleString("tr-TR")}</time>
                </li>
              ))}
            </ul>
          )}
        </section>
      </main>
    </div>
  );
}
