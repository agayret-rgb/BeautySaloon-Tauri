import { useCallback, useEffect, useMemo, useState } from "react";
import type { AppHealth, AppointmentSummary, CloudConnectionStatus, GoogleConnectionStatus } from "./tauriApi";
import {
  connectGoogleCalendar,
  disconnectCloud,
  disconnectGoogleCalendar,
  getAppHealth,
  getCloudConnectionStatus,
  getGoogleCalendarStatus,
  listAppointmentsByDate,
  processCloudOutbox,
  requestCloudOtp,
  syncGoogleCalendar,
  verifyCloudOtp
} from "./tauriApi";

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
  const [googleStatus, setGoogleStatus] = useState<GoogleConnectionStatus | null>(null);
  const [cloudStatus, setCloudStatus] = useState<CloudConnectionStatus | null>(null);
  const [serviceStatus, setServiceStatus] = useState("Servis durumu bekliyor");
  const [cloudEmail, setCloudEmail] = useState("");
  const [cloudOtp, setCloudOtp] = useState("");

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

  const refreshServiceStatus = useCallback(async () => {
    const [nextGoogleStatus, nextCloudStatus] = await Promise.all([getGoogleCalendarStatus(), getCloudConnectionStatus()]);
    setGoogleStatus(nextGoogleStatus);
    setCloudStatus(nextCloudStatus);
    setServiceStatus("Servis durumu guncel");
  }, []);

  useEffect(() => {
    void refreshCoreData().catch((error) => {
      setStatus(error instanceof Error ? error.message : "Native komut yanit vermedi");
    });
    void refreshServiceStatus().catch((error) => {
      setServiceStatus(error instanceof Error ? error.message : "Servis komutu yanit vermedi");
    });
  }, [refreshCoreData, refreshServiceStatus]);

  const runServiceAction = useCallback(
    async (action: () => Promise<unknown>, successMessage: string) => {
      setServiceStatus("Isleniyor");
      try {
        await action();
        await refreshServiceStatus();
        setServiceStatus(successMessage);
      } catch (error) {
        setServiceStatus(error instanceof Error ? error.message : "Servis islemi tamamlanamadi");
      }
    },
    [refreshServiceStatus]
  );

  const isSettings = activeItem === "Ayarlar";

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

        {isSettings ? (
          <section className="today-panel" aria-labelledby="settings-title">
            <div>
              <p className="eyebrow">Live services</p>
              <h2 id="settings-title">Baglanti sinirlari</h2>
              <p>Google Calendar ve bulut komutlari Rust tarafindan calisir; gizli degerler arayuze tasinmaz.</p>
            </div>
            <button type="button" className="secondary-action" onClick={() => void refreshServiceStatus()}>
              Durumu yenile
            </button>
          </section>
        ) : (
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
        )}

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

        {isSettings ? (
          <section className="settings-grid" aria-label="Canli servis ayarlari">
            <div className="settings-panel">
              <h2>Google Calendar</h2>
              <p>{googleStatus?.configured ? "Config bulundu" : "Config bekleniyor"}</p>
              <p>{googleStatus?.connected ? "Bagli" : "Bagli degil"} - {googleStatus?.pendingSyncCount ?? 0} bekleyen</p>
              <div className="service-actions">
                <button type="button" className="secondary-action" onClick={() => void runServiceAction(connectGoogleCalendar, "Google baglantisi tamamlandi")}>
                  Baglan
                </button>
                <button type="button" className="secondary-action" onClick={() => void runServiceAction(syncGoogleCalendar, "Google outbox islendi")}>
                  Senkronize et
                </button>
                <button type="button" className="secondary-action" onClick={() => void runServiceAction(disconnectGoogleCalendar, "Google baglantisi kesildi")}>
                  Baglantiyi kes
                </button>
              </div>
            </div>
            <div className="settings-panel">
              <h2>Bulut</h2>
              <p>{cloudStatus?.configured ? "Config bulundu" : "Config bekleniyor"}</p>
              <p>{cloudStatus?.sessionPresent ? "Oturum var" : "Oturum yok"}</p>
              <div className="field-row">
                <input aria-label="Bulut email" value={cloudEmail} onChange={(event) => setCloudEmail(event.target.value)} placeholder="email" />
                <button type="button" className="secondary-action" onClick={() => void runServiceAction(() => requestCloudOtp(cloudEmail), "OTP gonderildi")}>
                  Kod gonder
                </button>
              </div>
              <div className="field-row">
                <input aria-label="Bulut OTP" value={cloudOtp} onChange={(event) => setCloudOtp(event.target.value)} placeholder="kod" />
                <button type="button" className="secondary-action" onClick={() => void runServiceAction(() => verifyCloudOtp(cloudEmail, cloudOtp), "Bulut oturumu acildi")}>
                  Dogrula
                </button>
              </div>
              <div className="service-actions">
                <button type="button" className="secondary-action" onClick={() => void runServiceAction(processCloudOutbox, "Bulut outbox islendi")}>
                  Outbox isle
                </button>
                <button type="button" className="secondary-action" onClick={() => void runServiceAction(disconnectCloud, "Bulut oturumu kapandi")}>
                  Baglantiyi kes
                </button>
              </div>
            </div>
            <div className="settings-panel settings-panel--wide">
              <h2>Komut durumu</h2>
              <p>{serviceStatus}</p>
            </div>
          </section>
        ) : (
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
        )}
      </main>
    </div>
  );
}
