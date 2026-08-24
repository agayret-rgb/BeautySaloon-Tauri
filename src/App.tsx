import { useCallback, useEffect, useMemo, useState } from "react";
import type { AppHealth, AppointmentSummary } from "./tauriApi";
import { getAppHealth, listAppointmentsByDate } from "./tauriApi";

const navigationItems = ["Bugun", "Takvim", "Musteriler", "Hizmetler", "Personel", "Ayarlar"] as const;

function todayLocalDate(): string {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
}

export function App() {
  const [activeItem, setActiveItem] = useState<(typeof navigationItems)[number]>("Bugun");
  const [health, setHealth] = useState<AppHealth | null>(null);
  const [appointments, setAppointments] = useState<AppointmentSummary[]>([]);
  const [status, setStatus] = useState("Hazirlaniyor");

  const localDate = useMemo(() => todayLocalDate(), []);
  const todayLabel = useMemo(() => {
    return new Intl.DateTimeFormat("tr-TR", {
      weekday: "long",
      day: "numeric",
      month: "long"
    }).format(new Date());
  }, []);

  const refreshCoreData = useCallback(async () => {
    const [nextHealth, todaysAppointments] = await Promise.all([getAppHealth(), listAppointmentsByDate(localDate)]);
    setHealth(nextHealth);
    setAppointments(todaysAppointments);
    setStatus(nextHealth.ok ? "SQLite core hazir" : "Kontrol gerekiyor");
  }, [localDate]);

  useEffect(() => {
    void refreshCoreData().catch((error) => {
      setStatus(error instanceof Error ? error.message : "Native komut yanit vermedi");
    });
  }, [refreshCoreData]);

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
            <p className="eyebrow">Core data</p>
            <h2 id="today-title">Bugunun randevu akisi</h2>
            <p>Bu ekran Tauri Rust komutu ile lokal SQLite appointment sorgusunu cagirir.</p>
          </div>
          <button type="button" className="secondary-action" onClick={() => void refreshCoreData()}>
            Randevulari yenile
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
            <span>Local veri yolu</span>
            <strong>{health?.databasePath ?? "..."}</strong>
          </div>
        </section>

        <section className="notes-panel" aria-label="Bugunun randevulari">
          <h2>Bugunun randevulari</h2>
          {appointments.length === 0 ? (
            <p>Bugun icin lokal randevu kaydi yok.</p>
          ) : (
            <ul>
              {appointments.map((appointment) => (
                <li key={appointment.id}>
                  <span>{appointment.customerName} - {appointment.serviceNames.join(", ")}</span>
                  <time>{new Date(appointment.startAtUtc).toLocaleTimeString("tr-TR", { hour: "2-digit", minute: "2-digit" })}</time>
                </li>
              ))}
            </ul>
          )}
        </section>
      </main>
    </div>
  );
}
