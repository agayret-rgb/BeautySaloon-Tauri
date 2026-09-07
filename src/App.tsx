import { useCallback, useEffect, useMemo, useState } from "react";
import type {
  AppointmentInput,
  AppointmentSummary,
  BusinessProfile,
  Customer,
  CustomerHistoryPage,
  RepeatBookingSeed,
  ServiceCategory,
  ServiceItem,
  ServiceStatistic,
  Staff,
  StaffTimeOff,
  StaffWorkingHour,
} from "./tauriApi";
import {
  addStaffTimeOff,
  archiveCustomer,
  createAppointment,
  createManualBackup,
  createCustomer,
  createServiceCategory,
  createService,
  createStaff,
  deactivateService,
  deactivateStaff,
  connectGoogleCalendar,
  disconnectGoogleCalendar,
  getBusinessProfile,
  getOnboardingState,
  getCloudConnectionStatus,
  getReminderReadiness,
  getGoogleCalendarStatus,
  getCustomerHistory,
  getRepeatBookingSeed,
  isStaffScheduleUnrestricted,
  listActiveServices,
  listActiveStaff,
  listAppointmentsByDate,
  listAppointmentsByDateRange,
  listServiceCategories,
  listServices,
  listStaff,
  listStaffServiceIds,
  listStaffTimeOff,
  listStaffWorkingHours,
  reactivateCustomer,
  reactivateService,
  reactivateStaff,
  removeStaffTimeOff,
  openDataFolder,
  requestCloudOtp,
  setReminderAutomaticEnabled,
  restoreDatabaseBackup,
  searchCustomers,
  searchCustomersByStatus,
  selectBackupFile,
  setStaffServices as persistStaffServices,
  setStaffWorkingHours,
  serviceStatistics,
  updateAppointment,
  updateBusinessProfile,
  updateService,
  updateStaff,
  updateStaffTimeOff,
  verifyCloudOtp,
} from "./tauriApi";

const navigationItems = [
  "Bugün",
  "Takvim",
  "Müşteriler",
  "Hizmetler",
  "Personel",
  "Ayarlar",
] as const;

function todayLocalDate(): string {
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone: "Europe/Istanbul",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  }).formatToParts(new Date());
  const value = (type: string) =>
    parts.find((part) => part.type === type)?.value;
  return `${value("year")}-${value("month")}-${value("day")}`;
}

function addLocalDays(localDate: string, days: number): string {
  const [year, month, day] = localDate.split("-").map(Number);
  const value = new Date(Date.UTC(year, month - 1, day + days));
  return value.toISOString().slice(0, 10);
}

function monthRange(
  month: string,
): { startDate: string; endDate: string } | null {
  const match = /^(\d{4})-(\d{2})$/.exec(month);
  if (!match) return null;
  const year = Number(match[1]);
  const monthNumber = Number(match[2]);
  if (monthNumber < 1 || monthNumber > 12) return null;
  const lastDay = new Date(Date.UTC(year, monthNumber, 0));
  return {
    startDate: `${month}-01`,
    endDate: lastDay.toISOString().slice(0, 10),
  };
}

function weekStart(localDate: string): string {
  const [year, month, day] = localDate.split("-").map(Number);
  const value = new Date(Date.UTC(year, month - 1, day));
  const mondayOffset = (value.getUTCDay() + 6) % 7;
  return addLocalDays(localDate, -mondayOffset);
}

function dateLabel(localDate: string): string {
  const [year, month, day] = localDate.split("-").map(Number);
  return new Intl.DateTimeFormat("tr-TR", {
    weekday: "long",
    day: "numeric",
    month: "long",
  }).format(new Date(Date.UTC(year, month - 1, day)));
}

function timeInIstanbul(value: string): string {
  return new Intl.DateTimeFormat("tr-TR", {
    timeZone: "Europe/Istanbul",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).format(new Date(value));
}

function localDateTime(
  value: string,
): { localDate: string; localStartTime: string } | null {
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone: "Europe/Istanbul",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).formatToParts(new Date(value));
  const part = (type: string) =>
    parts.find((item) => item.type === type)?.value;
  const year = part("year");
  const month = part("month");
  const day = part("day");
  const hour = part("hour");
  const minute = part("minute");
  return year && month && day && hour && minute
    ? {
        localDate: `${year}-${month}-${day}`,
        localStartTime: `${hour}:${minute}`,
      }
    : null;
}

function statusLabel(status: string): string {
  return (
    {
      planned: "Planlandı",
      confirmed: "Onaylandı",
      completed: "Tamamlandı",
      no_show: "Gelmedi",
      cancelled: "İptal",
    }[status] ?? "Bilinmiyor"
  );
}

function bookingErrorMessage(error: unknown): string {
  const message =
    error instanceof Error ? error.message : typeof error === "string" ? error : "";
  if (message.includes("OUTSIDE_WORKING_HOURS"))
    return "Seçilen saat personelin çalışma saatleri dışında.";
  if (message.includes("STAFF_TIME_OFF"))
    return "Personel seçilen tarih veya saatte izinli.";
  if (message.includes("APPOINTMENT_CONFLICT"))
    return "Bu personelin seçilen saatte başka bir randevusu var.";
  if (message.includes("STAFF_INACTIVE"))
    return "Seçilen personel artık aktif değil.";
  if (
    message.includes("SERVICE_INACTIVE") ||
    message.includes("SERVICE_NOT_READY")
  )
    return "Seçilen hizmet artık randevu için uygun değil.";
  if (message.includes("SERVICE_NOT_ASSIGNED_TO_STAFF"))
    return "Seçilen personel bu hizmeti sunmuyor.";
  if (message.includes("CUSTOMER_INACTIVE"))
    return "Bu müşteri için önce tekrar aktif hale getirme işlemi gerekiyor.";
  if (
    message.includes("CUSTOMER_PHONE_CONFLICT") ||
    message.includes("PHONE_CONFLICT") ||
    message.includes("customers.phone")
  ) {
    return "Bu telefon numarası başka bir müşteride kayıtlı.";
  }
  return "İşlem tamamlanamadı. Bilgileri kontrol edip tekrar deneyin.";
}

function customerName(customer: Customer): string {
  return [customer.firstName, customer.lastName].filter(Boolean).join(" ");
}

function customerPhoneLabel(phone: string | null): string {
  if (!phone) return "Telefon yok";
  const digits = phone.replace(/\D/g, "");
  const canonical = digits.startsWith("90")
    ? digits.slice(2)
    : digits.startsWith("0")
      ? digits.slice(1)
      : digits;
  if (/^5\d{9}$/.test(canonical)) {
    return `0${canonical.slice(0, 3)} ${canonical.slice(3, 6)} ${canonical.slice(6, 8)} ${canonical.slice(8)}`;
  }
  return phone;
}

function staffName(staff: Staff): string {
  return [staff.firstName, staff.lastName].filter(Boolean).join(" ");
}

function minutesToTime(value: number): string {
  return `${String(Math.floor(value / 60)).padStart(2, "0")}:${String(value % 60).padStart(2, "0")}`;
}
function timeToMinutes(value: string): number | null {
  const match = /^(\d{2}):(\d{2})$/.exec(value);
  if (!match) return null;
  const minutes = Number(match[1]) * 60 + Number(match[2]);
  return minutes >= 0 && minutes <= 1440 ? minutes : null;
}
function parseMinor(value: string): number | null {
  const normalized = value.trim().replace(",", ".");
  if (!/^\d+(?:\.\d{1,2})?$/.test(normalized)) return null;
  const [whole, fraction = ""] = normalized.split(".");
  return Number(whole) * 100 + Number((fraction + "00").slice(0, 2));
}
function sameIds(left: string[], right: string[]): boolean {
  if (left.length !== right.length) return false;
  const sortedLeft = [...left].sort();
  const sortedRight = [...right].sort();
  return sortedLeft.every((value, index) => value === sortedRight[index]);
}
function formatMinor(value: number, currencyCode: string): string {
  return new Intl.NumberFormat("tr-TR", {
    style: "currency",
    currency: currencyCode,
  }).format(value / 100);
}
const weekdays = [
  "Pazartesi",
  "Salı",
  "Çarşamba",
  "Perşembe",
  "Cuma",
  "Cumartesi",
  "Pazar",
];

type CalendarMode = "day" | "week";

type BookingPrefill = {
  customer?: Customer;
  serviceIds?: string[];
  staffId?: string;
  localDate?: string;
  localStartTime?: string;
  notice?: string;
};

type BookingDraft = {
  customerId: string;
  customerQuery: string;
  phone: string;
  whatsappConsent: boolean;
  serviceIds: string[];
  staffId: string;
  localDate: string;
  localStartTime: string;
};

type AppointmentEditDraft = {
  localDate: string;
  localStartTime: string;
  staffId: string;
  serviceIds: string[];
};

export function App() {
  const [activeItem, setActiveItem] =
    useState<(typeof navigationItems)[number]>("Bugün");
  const [appointments, setAppointments] = useState<AppointmentSummary[]>([]);
  const [todayLoading, setTodayLoading] = useState(true);
  const [todayError, setTodayError] = useState("");
  const [statusPendingId, setStatusPendingId] = useState<string | null>(null);
  const [calendarMode, setCalendarMode] = useState<CalendarMode>("day");
  const [calendarDate, setCalendarDate] = useState(todayLocalDate());
  const [calendarAppointments, setCalendarAppointments] = useState<
    AppointmentSummary[]
  >([]);
  const [calendarLoading, setCalendarLoading] = useState(false);
  const [calendarError, setCalendarError] = useState("");
  const [editingAppointment, setEditingAppointment] =
    useState<AppointmentSummary | null>(null);
  const [editDate, setEditDate] = useState("");
  const [editTime, setEditTime] = useState("");
  const [editStaffId, setEditStaffId] = useState("");
  const [editServiceIds, setEditServiceIds] = useState<string[]>([]);
  const [editStaff, setEditStaff] = useState<Staff[]>([]);
  const [editServices, setEditServices] = useState<ServiceItem[]>([]);
  const [editError, setEditError] = useState("");
  const [isEditSaving, setIsEditSaving] = useState(false);
  const [customerList, setCustomerList] = useState<Customer[]>([]);
  const [customerSearch, setCustomerSearch] = useState("");
  const [showArchivedCustomers, setShowArchivedCustomers] = useState(false);
  const [customersLoading, setCustomersLoading] = useState(false);
  const [customersError, setCustomersError] = useState("");
  const [customerHistory, setCustomerHistory] =
    useState<CustomerHistoryPage | null>(null);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyError, setHistoryError] = useState("");
  const [historyOffset, setHistoryOffset] = useState(0);
  const [historyHasMore, setHistoryHasMore] = useState(false);
  const [historyPageLoading, setHistoryPageLoading] = useState(false);
  const [customerActionPending, setCustomerActionPending] = useState(false);
  const [bookingOpen, setBookingOpen] = useState(false);
  const [bookingInitialDraft, setBookingInitialDraft] =
    useState<BookingDraft | null>(null);
  const [customerPickerOpen, setCustomerPickerOpen] = useState(false);
  const [customerQuery, setCustomerQuery] = useState("");
  const [customerResults, setCustomerResults] = useState<Customer[]>([]);
  const [selectedCustomer, setSelectedCustomer] = useState<Customer | null>(
    null,
  );
  const [phone, setPhone] = useState("");
  const [whatsappConsent, setWhatsappConsent] = useState(false);
  const [services, setServices] = useState<ServiceItem[]>([]);
  const [staff, setStaff] = useState<Staff[]>([]);
  const [serviceIds, setServiceIds] = useState<string[]>([]);
  const [staffId, setStaffId] = useState("");
  const [localDate, setLocalDate] = useState(todayLocalDate());
  const [localStartTime, setLocalStartTime] = useState("10:00");
  const [formError, setFormError] = useState("");
  const [formNotice, setFormNotice] = useState("");
  const [isSaving, setIsSaving] = useState(false);
  const [editInitialDraft, setEditInitialDraft] =
    useState<AppointmentEditDraft | null>(null);
  const [pendingRootNavigation, setPendingRootNavigation] = useState<
    (typeof navigationItems)[number] | null
  >(null);
  const [currencyCode, setCurrencyCode] = useState("TRY");
  const [adminServices, setAdminServices] = useState<ServiceItem[]>([]);
  const [serviceCategories, setServiceCategories] = useState<ServiceCategory[]>(
    [],
  );
  const [showInactiveServices, setShowInactiveServices] = useState(false);
  const [servicesAdminLoading, setServicesAdminLoading] = useState(false);
  const [servicesAdminError, setServicesAdminError] = useState("");
  const [servicesView, setServicesView] = useState<"manage" | "statistics">(
    "manage",
  );
  const [statisticsMode, setStatisticsMode] = useState<"month" | "range">(
    "month",
  );
  const [statisticsMonth, setStatisticsMonth] = useState(
    todayLocalDate().slice(0, 7),
  );
  const [statisticsStartDate, setStatisticsStartDate] =
    useState(todayLocalDate());
  const [statisticsEndDate, setStatisticsEndDate] = useState(todayLocalDate());
  const [serviceStatisticsRows, setServiceStatisticsRows] = useState<
    ServiceStatistic[]
  >([]);
  const [statisticsLoading, setStatisticsLoading] = useState(false);
  const [statisticsError, setStatisticsError] = useState("");
  const [editingService, setEditingService] = useState<ServiceItem | null>(
    null,
  );
  const [serviceName, setServiceName] = useState("");
  const [serviceCategoryId, setServiceCategoryId] = useState("");
  const [serviceDuration, setServiceDuration] = useState("30");
  const [servicePrice, setServicePrice] = useState("0");
  const [serviceSaving, setServiceSaving] = useState(false);
  const [adminStaff, setAdminStaff] = useState<Staff[]>([]);
  const [showInactiveStaff, setShowInactiveStaff] = useState(false);
  const [staffAdminLoading, setStaffAdminLoading] = useState(false);
  const [staffAdminError, setStaffAdminError] = useState("");
  const [editingStaff, setEditingStaff] = useState<Staff | null>(null);
  const [staffFirstName, setStaffFirstName] = useState("");
  const [staffLastName, setStaffLastName] = useState("");
  const [staffPhone, setStaffPhone] = useState("");
  const [staffSpecialtyNote, setStaffSpecialtyNote] = useState("");
  const [staffSaving, setStaffSaving] = useState(false);
  const [staffDetail, setStaffDetail] = useState<Staff | null>(null);
  const [staffDetailLoading, setStaffDetailLoading] = useState(false);
  const [staffServiceIds, setStaffServiceIds] = useState<string[]>([]);
  const [staffServices, setStaffServices] = useState<ServiceItem[]>([]);
  const [workingHours, setWorkingHours] = useState<StaffWorkingHour[]>([]);
  const [scheduleUnrestricted, setScheduleUnrestricted] = useState(true);
  const [timeOffItems, setTimeOffItems] = useState<StaffTimeOff[]>([]);
  const [timeOffDate, setTimeOffDate] = useState(todayLocalDate());
  const [timeOffFullDay, setTimeOffFullDay] = useState(true);
  const [timeOffStart, setTimeOffStart] = useState("09:00");
  const [timeOffEnd, setTimeOffEnd] = useState("10:00");
  const [editingTimeOffId, setEditingTimeOffId] = useState<string | null>(null);
  const [businessProfile, setBusinessProfile] =
    useState<BusinessProfile | null>(null);
  const [profileLoading, setProfileLoading] = useState(false);
  const [profileSaving, setProfileSaving] = useState(false);
  const [profileError, setProfileError] = useState("");
  const [profileNotice, setProfileNotice] = useState("");
  const [businessName, setBusinessName] = useState("");
  const [businessPhone, setBusinessPhone] = useState("");
  const [businessEmail, setBusinessEmail] = useState("");
  const [businessAddress, setBusinessAddress] = useState("");
  const [businessCurrency, setBusinessCurrency] = useState("TRY");
  const [backupSaving, setBackupSaving] = useState(false);
  const [backupNotice, setBackupNotice] = useState("");
  const [backupError, setBackupError] = useState("");
  const [restoreCandidate, setRestoreCandidate] = useState<string | null>(null);
  const [restoreConfirming, setRestoreConfirming] = useState(false);
  const [restoreSaving, setRestoreSaving] = useState(false);
  const [restoreNotice, setRestoreNotice] = useState("");
  const [restoreError, setRestoreError] = useState("");
  const [dataFolderError, setDataFolderError] = useState("");
  const [googleConnected, setGoogleConnected] = useState(false);
  const [googleLoading, setGoogleLoading] = useState(false);
  const [googleConnecting, setGoogleConnecting] = useState(false);
  const [googleDisconnecting, setGoogleDisconnecting] = useState(false);
  const [googleError, setGoogleError] = useState("");
  const [googleNotice, setGoogleNotice] = useState("");
  const [reminderConnected, setReminderConnected] = useState(false);
  const [reminderConnectionUnavailable, setReminderConnectionUnavailable] =
    useState(false);
  const [reminderReadiness, setReminderReadiness] = useState<
    "disconnected" | "connected_not_ready" | "ready"
  >("disconnected");
  const [automaticRemindersEnabled, setAutomaticRemindersEnabled] =
    useState(false);
  const [reminderSettingSaving, setReminderSettingSaving] = useState(false);
  const [reminderLoading, setReminderLoading] = useState(false);
  const [reminderEmail, setReminderEmail] = useState("");
  const [reminderCode, setReminderCode] = useState("");
  const [reminderCodeSent, setReminderCodeSent] = useState(false);
  const [reminderRequesting, setReminderRequesting] = useState(false);
  const [reminderVerifying, setReminderVerifying] = useState(false);
  const [reminderError, setReminderError] = useState("");
  const [reminderNotice, setReminderNotice] = useState("");
  const [onboardingLoading, setOnboardingLoading] = useState(false);
  const [onboardingOpen, setOnboardingOpen] = useState(false);
  const [onboardingStep, setOnboardingStep] = useState(1);
  const [onboardingStaffId, setOnboardingStaffId] = useState("");
  const [onboardingStaffName, setOnboardingStaffName] = useState("");
  const [onboardingServiceName, setOnboardingServiceName] = useState("");
  const [onboardingServiceDuration, setOnboardingServiceDuration] =
    useState("30");
  const [onboardingServicePrice, setOnboardingServicePrice] = useState("0");
  const [onboardingSaving, setOnboardingSaving] = useState(false);
  const [onboardingError, setOnboardingError] = useState("");

  const todayLabel = useMemo(
    () =>
      new Intl.DateTimeFormat("tr-TR", {
        weekday: "long",
        day: "numeric",
        month: "long",
      }).format(new Date()),
    [],
  );

  const loadToday = useCallback(async () => {
    setTodayLoading(true);
    setTodayError("");
    try {
      setAppointments(await listAppointmentsByDate(todayLocalDate()));
    } catch {
      setTodayError("Bugünün randevuları yüklenemedi.");
    } finally {
      setTodayLoading(false);
    }
  }, []);

  const calendarRange = useMemo(() => {
    if (calendarMode === "day")
      return { startDate: calendarDate, endDate: calendarDate };
    const startDate = weekStart(calendarDate);
    return { startDate, endDate: addLocalDays(startDate, 6) };
  }, [calendarDate, calendarMode]);

  const loadCalendar = useCallback(async () => {
    setCalendarLoading(true);
    setCalendarError("");
    try {
      setCalendarAppointments(
        await listAppointmentsByDateRange(
          calendarRange.startDate,
          calendarRange.endDate,
        ),
      );
    } catch {
      setCalendarError("Takvim randevuları yüklenemedi.");
    } finally {
      setCalendarLoading(false);
    }
  }, [calendarRange]);

  const loadCustomers = useCallback(async () => {
    setCustomersLoading(true);
    setCustomersError("");
    try {
      setCustomerList(
        await searchCustomersByStatus(
          customerSearch.trim(),
          showArchivedCustomers ? "all" : "active",
        ),
      );
    } catch {
      setCustomersError("Müşteriler yüklenemedi.");
    } finally {
      setCustomersLoading(false);
    }
  }, [customerSearch, showArchivedCustomers]);

  const loadCustomerHistory = useCallback(
    async (customerId: string, offset = 0, append = false) => {
      if (append) setHistoryPageLoading(true);
      else setHistoryLoading(true);
      setHistoryError("");
      try {
        const page = await getCustomerHistory(customerId, 10, offset);
        setCustomerHistory((current) =>
          append && current
            ? {
                ...page,
                appointments: [...current.appointments, ...page.appointments],
              }
            : page,
        );
        setHistoryOffset(offset);
        setHistoryHasMore(page.appointments.length === page.limit);
      } catch {
        setHistoryError("Randevu geçmişi yüklenemedi.");
      } finally {
        setHistoryLoading(false);
        setHistoryPageLoading(false);
      }
    },
    [],
  );

  useEffect(() => {
    void loadToday();
  }, [loadToday]);

  useEffect(() => {
    let active = true;
    void (async () => {
      try {
        const state = await getOnboardingState();
        if (!active) return;
        setOnboardingOpen(state.needsOnboarding);
        setOnboardingStep(state.nextStep);
        if (state.needsOnboarding && state.nextStep >= 3) {
          const existingStaff = await listActiveStaff();
          if (active) setOnboardingStaffId(existingStaff[0]?.id ?? "");
        }
      } catch {
        if (active) setOnboardingOpen(false);
      } finally {
        if (active) setOnboardingLoading(false);
      }
    })();
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (activeItem === "Takvim") void loadCalendar();
  }, [activeItem, loadCalendar]);

  useEffect(() => {
    if (activeItem !== "Müşteriler") return;
    const timer = window.setTimeout(() => void loadCustomers(), 180);
    return () => window.clearTimeout(timer);
  }, [activeItem, loadCustomers]);

  const loadServicesAdmin = useCallback(async () => {
    setServicesAdminLoading(true);
    setServicesAdminError("");
    try {
      const [items, categories, profile] = await Promise.all([
        listServices(showInactiveServices ? "all" : "active"),
        listServiceCategories(),
        getBusinessProfile(),
      ]);
      setAdminServices(items);
      setServiceCategories(categories);
      setCurrencyCode(profile.currencyCode);
      setServiceCategoryId((current) => current || categories[0]?.id || "");
    } catch {
      setServicesAdminError("Hizmetler yüklenemedi.");
    } finally {
      setServicesAdminLoading(false);
    }
  }, [showInactiveServices]);

  const loadServiceStatistics = useCallback(
    async (startDate: string, endDate: string) => {
      if (!startDate || !endDate || startDate > endDate) {
        setStatisticsError("Başlangıç tarihi bitiş tarihinden sonra olamaz.");
        return;
      }
      setStatisticsLoading(true);
      setStatisticsError("");
      setServiceStatisticsRows([]);
      try {
        setServiceStatisticsRows(await serviceStatistics(startDate, endDate));
      } catch {
        setStatisticsError("İstatistikler yüklenemedi.");
      } finally {
        setStatisticsLoading(false);
      }
    },
    [],
  );

  const loadStaffAdmin = useCallback(async () => {
    setStaffAdminLoading(true);
    setStaffAdminError("");
    try {
      setAdminStaff(await listStaff(showInactiveStaff ? "all" : "active"));
    } catch {
      setStaffAdminError("Personel listesi yüklenemedi.");
    } finally {
      setStaffAdminLoading(false);
    }
  }, [showInactiveStaff]);

  const applyBusinessProfile = useCallback((profile: BusinessProfile) => {
    setBusinessProfile(profile);
    setBusinessName(profile.businessName);
    setBusinessPhone(profile.phone ?? "");
    setBusinessEmail(profile.email ?? "");
    setBusinessAddress(profile.address ?? "");
    setBusinessCurrency(profile.currencyCode);
    setCurrencyCode(profile.currencyCode);
  }, []);

  const loadBusinessProfile = useCallback(async () => {
    setProfileLoading(true);
    setProfileError("");
    try {
      applyBusinessProfile(await getBusinessProfile());
    } catch {
      setProfileError("İşletme bilgileri yüklenemedi.");
    } finally {
      setProfileLoading(false);
    }
  }, [applyBusinessProfile]);

  const loadGoogleConnection = useCallback(async () => {
    setGoogleLoading(true);
    try {
      const status = await getGoogleCalendarStatus();
      setGoogleConnected(status.connected);
    } catch {
      setGoogleError("Google Takvim durumu yüklenemedi.");
    } finally {
      setGoogleLoading(false);
    }
  }, []);

  const loadReminderConnection = useCallback(async () => {
    setReminderLoading(true);
    try {
      const status = await getCloudConnectionStatus();
      const readiness = await getReminderReadiness();
      setReminderConnected(
        status.sessionPresent && readiness.state !== "disconnected",
      );
      setReminderConnectionUnavailable(
        status.validationState === "unavailable",
      );
      setReminderReadiness(readiness.state);
      setAutomaticRemindersEnabled(readiness.automaticEnabled);
    } catch {
      setReminderError("Hatırlatma bağlantısı yüklenemedi.");
    } finally {
      setReminderLoading(false);
    }
  }, []);

  const loadStaffDetail = useCallback(async (staffMember: Staff) => {
    setStaffDetail(staffMember);
    setStaffDetailLoading(true);
    setStaffAdminError("");
    try {
      const [
        assignedServiceIds,
        activeServices,
        intervals,
        unrestricted,
        timeOff,
      ] = await Promise.all([
        listStaffServiceIds(staffMember.id),
        listActiveServices(),
        listStaffWorkingHours(staffMember.id),
        isStaffScheduleUnrestricted(staffMember.id),
        listStaffTimeOff(staffMember.id),
      ]);
      setStaffServiceIds(assignedServiceIds);
      setStaffServices(activeServices);
      setWorkingHours(intervals);
      setScheduleUnrestricted(unrestricted);
      setTimeOffItems(timeOff);
      setEditingTimeOffId(null);
      setTimeOffFullDay(true);
    } catch {
      setStaffAdminError("Personel ayarları yüklenemedi.");
    } finally {
      setStaffDetailLoading(false);
    }
  }, []);

  useEffect(() => {
    if (activeItem === "Hizmetler") void loadServicesAdmin();
  }, [activeItem, loadServicesAdmin]);

  useEffect(() => {
    if (activeItem !== "Hizmetler" || servicesView !== "statistics") return;
    const range = monthRange(statisticsMonth);
    if (statisticsMode === "month" && range)
      void loadServiceStatistics(range.startDate, range.endDate);
  }, [
    activeItem,
    loadServiceStatistics,
    servicesView,
    statisticsMode,
    statisticsMonth,
  ]);

  useEffect(() => {
    if (activeItem === "Personel") void loadStaffAdmin();
  }, [activeItem, loadStaffAdmin]);

  useEffect(() => {
    if (activeItem !== "Ayarlar") return;
    void loadBusinessProfile();
    void loadGoogleConnection();
    void loadReminderConnection();
  }, [
    activeItem,
    loadBusinessProfile,
    loadGoogleConnection,
    loadReminderConnection,
  ]);

  useEffect(() => {
    let current = true;
    if (
      !bookingOpen ||
      selectedCustomer ||
      (!customerPickerOpen && customerQuery.trim().length < 2)
    ) {
      setCustomerResults([]);
      return;
    }
    const query = customerQuery.trim();
    const timer = window.setTimeout(() => {
      void searchCustomers(query)
        .then((items) => {
          if (current) setCustomerResults(items.filter((item) => item.isActive));
        })
        .catch(() => {
          if (current) setCustomerResults([]);
        });
    }, 220);
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [bookingOpen, customerPickerOpen, customerQuery, selectedCustomer]);

  const resetBooking = useCallback(() => {
    setCustomerQuery("");
    setCustomerResults([]);
    setSelectedCustomer(null);
    setBookingInitialDraft(null);
    setCustomerPickerOpen(false);
    setPhone("");
    setWhatsappConsent(false);
    setServiceIds([]);
    setStaffId("");
    setLocalDate(todayLocalDate());
    setLocalStartTime("10:00");
    setFormError("");
    setFormNotice("");
  }, []);

  const openBooking = useCallback(
    async (prefillDate = todayLocalDate(), prefill?: BookingPrefill) => {
      const customer = prefill?.customer ?? null;
      const nextServiceIds = prefill?.serviceIds ?? [];
      const nextStaffId = prefill?.staffId ?? "";
      const nextLocalDate = prefill?.localDate ?? prefillDate;
      const nextLocalStartTime = prefill?.localStartTime ?? "10:00";
      setBookingOpen(true);
      setFormError("");
      setSelectedCustomer(customer);
      setCustomerQuery(customer ? customerName(customer) : "");
      setPhone(customer?.phone ?? "");
      setWhatsappConsent(false);
      setServiceIds(nextServiceIds);
      setStaffId(nextStaffId);
      setLocalDate(nextLocalDate);
      setLocalStartTime(nextLocalStartTime);
      setBookingInitialDraft({
        customerId: customer?.id ?? "",
        customerQuery: customer ? customerName(customer) : "",
        phone: customer?.phone ?? "",
        whatsappConsent: false,
        serviceIds: nextServiceIds,
        staffId: nextStaffId,
        localDate: nextLocalDate,
        localStartTime: nextLocalStartTime,
      });
      setCustomerPickerOpen(false);
      setFormNotice(prefill?.notice ?? "");
      try {
        const [nextServices, nextStaff] = await Promise.all([
          listActiveServices(),
          listActiveStaff(),
        ]);
        setServices(nextServices.filter((item) => item.isActive));
        setStaff(nextStaff.filter((item) => item.isActive));
        if (
          prefill?.staffId &&
          !nextStaff.some(
            (item) => item.isActive && item.id === prefill.staffId,
          )
        ) {
          setStaffId("");
          setBookingInitialDraft((current) =>
            current ? { ...current, staffId: "" } : current,
          );
        }
      } catch {
        setFormError("Randevu seçenekleri yüklenemedi.");
      }
    },
    [],
  );

  const customerForBooking = useCallback(
    (history: CustomerHistoryPage): Customer => {
      const parts = history.customer.name.split(/\s+/).filter(Boolean);
      return {
        id: history.customer.customerId,
        firstName: parts[0] ?? "",
        lastName: parts.slice(1).join(" "),
        phone: history.customer.phone,
        isActive: history.customer.isActive,
      };
    },
    [],
  );

  const openRepeatBooking = useCallback(
    async (sourceAppointmentId: string) => {
      if (!customerHistory) return;
      try {
        const seed: RepeatBookingSeed = await getRepeatBookingSeed(
          customerHistory.customer.customerId,
          sourceAppointmentId,
        );
        const unavailable = seed.services.filter(
          (service) => !service.isEligible,
        );
        await openBooking("", {
          customer: customerForBooking(customerHistory),
          serviceIds: seed.services
            .filter((service) => service.isEligible)
            .map((service) => service.serviceId),
          staffId: seed.staffId ?? "",
          localDate: "",
          localStartTime: "",
          notice:
            unavailable.length > 0
              ? "Bu hizmet artık aktif değil. Yeni bir hizmet seçin."
              : "",
        });
      } catch {
        setHistoryError("Tekrar randevu bilgisi hazırlanamadı.");
      }
    },
    [customerForBooking, customerHistory, openBooking],
  );

  const updateCustomerArchiveState = useCallback(async () => {
    if (!customerHistory || customerActionPending) return;
    setCustomerActionPending(true);
    setHistoryError("");
    try {
      if (customerHistory.customer.isActive)
        await archiveCustomer(customerHistory.customer.customerId);
      else await reactivateCustomer(customerHistory.customer.customerId);
      await Promise.all([
        loadCustomers(),
        loadCustomerHistory(customerHistory.customer.customerId),
      ]);
    } catch {
      setHistoryError("Müşteri durumu güncellenemedi.");
    } finally {
      setCustomerActionPending(false);
    }
  }, [
    customerActionPending,
    customerHistory,
    loadCustomerHistory,
    loadCustomers,
  ]);

  const saveAppointment = useCallback(async () => {
    if (isSaving) return;
    const nameParts = customerQuery.trim().split(/\s+/).filter(Boolean);
    if (!selectedCustomer && nameParts.length < 2) {
      setFormError("Müşteri için ad ve soyad girin.");
      return;
    }
    if (!staffId || serviceIds.length === 0 || !localDate || !localStartTime) {
      setFormError("Müşteri, hizmet, personel, tarih ve saat seçin.");
      return;
    }
    setIsSaving(true);
    setFormError("");
    try {
      let customer = selectedCustomer;
      if (!customer) {
        customer = await createCustomer({
          firstName: nameParts[0],
          lastName: nameParts.slice(1).join(" "),
          phone: phone.trim() || null,
          whatsappConsentConfirmed: whatsappConsent,
        });
        setSelectedCustomer(customer);
      }
      await createAppointment({
        customerId: customer.id,
        staffId,
        localDate,
        localStartTime,
        serviceIds,
        status: "planned",
        note: null,
      });
      resetBooking();
      setBookingOpen(false);
      setFormNotice("Randevu kaydedildi.");
      await Promise.all([
        loadToday(),
        activeItem === "Takvim" ? loadCalendar() : Promise.resolve(),
      ]);
      if (customerHistory?.customer.customerId === customer.id)
        await loadCustomerHistory(customer.id);
    } catch (error) {
      setFormError(bookingErrorMessage(error));
    } finally {
      setIsSaving(false);
    }
  }, [
    activeItem,
    customerHistory?.customer.customerId,
    customerQuery,
    isSaving,
    loadCalendar,
    loadCustomerHistory,
    loadToday,
    localDate,
    localStartTime,
    phone,
    whatsappConsent,
    resetBooking,
    selectedCustomer,
    serviceIds,
    staffId,
  ]);

  const changeStatus = useCallback(
    async (appointment: AppointmentSummary, status: string) => {
      if (statusPendingId) return;
      const local = localDateTime(appointment.startAtUtc);
      if (!local) {
        setTodayError("Randevu saati işlenemedi.");
        return;
      }
      setStatusPendingId(appointment.id);
      setTodayError("");
      const input: AppointmentInput = {
        customerId: appointment.customerId,
        staffId: appointment.staffId,
        localDate: local.localDate,
        localStartTime: local.localStartTime,
        serviceIds: appointment.services.map((service) => service.serviceId),
        status,
        note: appointment.note,
      };
      try {
        await updateAppointment(appointment.id, input);
        await Promise.all([
          loadToday(),
          activeItem === "Takvim" ? loadCalendar() : Promise.resolve(),
        ]);
      } catch {
        setTodayError("Randevu durumu güncellenemedi.");
      } finally {
        setStatusPendingId(null);
      }
    },
    [activeItem, loadCalendar, loadToday, statusPendingId],
  );

  const openAppointmentEditor = useCallback(
    async (appointment: AppointmentSummary) => {
      const local = localDateTime(appointment.startAtUtc);
      if (!local) {
        setCalendarError("Randevu saati işlenemedi.");
        return;
      }
      setEditingAppointment(appointment);
      setEditDate(local.localDate);
      setEditTime(local.localStartTime);
      setEditStaffId(appointment.staffId);
      const serviceIds = appointment.services.map((service) => service.serviceId);
      setEditServiceIds(serviceIds);
      setEditInitialDraft({
        localDate: local.localDate,
        localStartTime: local.localStartTime,
        staffId: appointment.staffId,
        serviceIds,
      });
      setEditError("");
      try {
        const [nextStaff, nextServices] = await Promise.all([
          listActiveStaff(),
          listActiveServices(),
        ]);
        setEditStaff(nextStaff.filter((item) => item.isActive));
        setEditServices(nextServices.filter((item) => item.isActive));
      } catch {
        setEditError("Randevu seçenekleri yüklenemedi.");
      }
    },
    [],
  );

  const resetAppointmentEditor = useCallback(() => {
    setEditingAppointment(null);
    setEditInitialDraft(null);
    setEditError("");
    setEditDate("");
    setEditTime("");
    setEditStaffId("");
    setEditServiceIds([]);
  }, []);

  const saveAppointmentEdit = useCallback(async () => {
    if (!editingAppointment || isEditSaving) return;
    if (!editDate || !editTime || !editStaffId || editServiceIds.length === 0) {
      setEditError("Personel, tarih, saat ve en az bir hizmet seçin.");
      return;
    }
    setIsEditSaving(true);
    setEditError("");
    try {
      await updateAppointment(editingAppointment.id, {
        customerId: editingAppointment.customerId,
        staffId: editStaffId,
        localDate: editDate,
        localStartTime: editTime,
        serviceIds: editServiceIds,
        status: editingAppointment.status,
        note: editingAppointment.note,
      });
      resetAppointmentEditor();
      await Promise.all([loadToday(), loadCalendar()]);
    } catch (error) {
      setEditError(bookingErrorMessage(error));
    } finally {
      setIsEditSaving(false);
    }
  }, [
    editDate,
    editServiceIds,
    editStaffId,
    editTime,
    editingAppointment,
    isEditSaving,
    loadCalendar,
    loadToday,
    resetAppointmentEditor,
  ]);

  const resetServiceForm = useCallback(() => {
    setEditingService(null);
    setServiceName("");
    setServiceDuration("30");
    setServicePrice("0");
    setServiceCategoryId(serviceCategories[0]?.id ?? "");
  }, [serviceCategories]);

  const beginServiceEdit = useCallback((item: ServiceItem) => {
    setEditingService(item);
    setServiceName(item.name);
    setServiceCategoryId(item.categoryId);
    setServiceDuration(String(item.durationMinutes ?? 0));
    setServicePrice((item.defaultPriceMinor / 100).toFixed(2));
    setServicesAdminError("");
  }, []);

  const saveService = useCallback(async () => {
    if (serviceSaving) return;
    const duration = Number(serviceDuration);
    const price = parseMinor(servicePrice);
    if (
      !serviceName.trim() ||
      !serviceCategoryId ||
      !Number.isInteger(duration) ||
      duration <= 0 ||
      price === null
    ) {
      setServicesAdminError(
        "Hizmet adı, kategori, süre ve geçerli bir fiyat girin.",
      );
      return;
    }
    setServiceSaving(true);
    setServicesAdminError("");
    const input = {
      categoryId: serviceCategoryId,
      name: serviceName.trim(),
      durationMinutes: duration,
      defaultPriceMinor: price,
      isActive: editingService?.isActive ?? true,
    };
    try {
      if (editingService) await updateService(editingService.id, input);
      else await createService(input);
      resetServiceForm();
      await loadServicesAdmin();
    } catch {
      setServicesAdminError(
        "Hizmet kaydedilemedi. Aynı adla aktif bir hizmet olabilir.",
      );
    } finally {
      setServiceSaving(false);
    }
  }, [
    editingService,
    loadServicesAdmin,
    resetServiceForm,
    serviceCategoryId,
    serviceDuration,
    serviceName,
    servicePrice,
    serviceSaving,
  ]);

  const setServiceActive = useCallback(
    async (item: ServiceItem) => {
      if (serviceSaving) return;
      setServiceSaving(true);
      setServicesAdminError("");
      try {
        if (item.isActive) await deactivateService(item.id);
        else await reactivateService(item.id);
        await loadServicesAdmin();
      } catch {
        setServicesAdminError("Hizmet durumu güncellenemedi.");
      } finally {
        setServiceSaving(false);
      }
    },
    [loadServicesAdmin, serviceSaving],
  );

  const resetStaffForm = useCallback(() => {
    setEditingStaff(null);
    setStaffFirstName("");
    setStaffLastName("");
    setStaffPhone("");
    setStaffSpecialtyNote("");
  }, []);

  const bookingIsDirty =
    bookingOpen &&
    bookingInitialDraft !== null &&
    (bookingInitialDraft.customerId !== (selectedCustomer?.id ?? "") ||
      bookingInitialDraft.customerQuery !== customerQuery ||
      bookingInitialDraft.phone !== phone ||
      bookingInitialDraft.whatsappConsent !== whatsappConsent ||
      !sameIds(bookingInitialDraft.serviceIds, serviceIds) ||
      bookingInitialDraft.staffId !== staffId ||
      bookingInitialDraft.localDate !== localDate ||
      bookingInitialDraft.localStartTime !== localStartTime);

  const appointmentEditIsDirty =
    editingAppointment !== null &&
    editInitialDraft !== null &&
    (editInitialDraft.localDate !== editDate ||
      editInitialDraft.localStartTime !== editTime ||
      editInitialDraft.staffId !== editStaffId ||
      !sameIds(editInitialDraft.serviceIds, editServiceIds));

  const completeRootNavigation = useCallback(
    (item: (typeof navigationItems)[number]) => {
      setBookingOpen(false);
      resetBooking();
      resetAppointmentEditor();
      setCustomerHistory(null);
      setHistoryError("");
      setHistoryOffset(0);
      setHistoryHasMore(false);
      resetServiceForm();
      resetStaffForm();
      setStaffDetail(null);
      setEditingTimeOffId(null);
      setRestoreConfirming(false);
      setRestoreCandidate(null);
      setActiveItem(item);
    },
    [
      resetAppointmentEditor,
      resetBooking,
      resetServiceForm,
      resetStaffForm,
    ],
  );

  const navigateToRoot = useCallback(
    (item: (typeof navigationItems)[number]) => {
      if (bookingIsDirty || appointmentEditIsDirty) {
        setPendingRootNavigation(item);
        return;
      }
      completeRootNavigation(item);
    },
    [appointmentEditIsDirty, bookingIsDirty, completeRootNavigation],
  );

  const beginStaffEdit = useCallback((item: Staff) => {
    setEditingStaff(item);
    setStaffFirstName(item.firstName);
    setStaffLastName(item.lastName ?? "");
    setStaffPhone(item.phone ?? "");
    setStaffSpecialtyNote(item.specialtyNote ?? "");
    setStaffAdminError("");
  }, []);

  const saveStaff = useCallback(async () => {
    if (staffSaving || !staffFirstName.trim()) {
      if (!staffFirstName.trim()) setStaffAdminError("Personel adı girin.");
      return;
    }
    setStaffSaving(true);
    setStaffAdminError("");
    const input = {
      firstName: staffFirstName.trim(),
      lastName: staffLastName.trim() || null,
      phone: staffPhone.trim() || null,
      specialtyNote: staffSpecialtyNote.trim() || null,
      colorKey: editingStaff?.colorKey ?? "sage",
      isActive: editingStaff?.isActive ?? true,
    };
    try {
      const saved = editingStaff
        ? await updateStaff(editingStaff.id, input)
        : await createStaff(input);
      resetStaffForm();
      await loadStaffAdmin();
      if (staffDetail?.id === saved.id) await loadStaffDetail(saved);
    } catch {
      setStaffAdminError("Personel kaydedilemedi.");
    } finally {
      setStaffSaving(false);
    }
  }, [
    editingStaff,
    loadStaffAdmin,
    loadStaffDetail,
    resetStaffForm,
    staffDetail?.id,
    staffFirstName,
    staffLastName,
    staffPhone,
    staffSaving,
    staffSpecialtyNote,
  ]);

  const setStaffActive = useCallback(
    async (item: Staff) => {
      if (staffSaving) return;
      setStaffSaving(true);
      setStaffAdminError("");
      try {
        const updated = item.isActive
          ? await deactivateStaff(item.id)
          : await reactivateStaff(item.id);
        await loadStaffAdmin();
        if (staffDetail?.id === item.id) await loadStaffDetail(updated);
      } catch {
        setStaffAdminError("Personel durumu güncellenemedi.");
      } finally {
        setStaffSaving(false);
      }
    },
    [loadStaffAdmin, loadStaffDetail, staffDetail?.id, staffSaving],
  );

  const saveStaffServices = useCallback(async () => {
    if (!staffDetail || staffSaving) return;
    setStaffSaving(true);
    try {
      setStaffServiceIds(
        await persistStaffServices(staffDetail.id, staffServiceIds),
      );
    } catch {
      setStaffAdminError("Sunulan hizmetler kaydedilemedi.");
    } finally {
      setStaffSaving(false);
    }
  }, [staffDetail, staffSaving, staffServiceIds]);

  const saveWorkingHours = useCallback(async () => {
    if (!staffDetail || staffSaving) return;
    setStaffSaving(true);
    setStaffAdminError("");
    try {
      await setStaffWorkingHours(
        staffDetail.id,
        workingHours.map((item) => ({ ...item, staffId: staffDetail.id })),
      );
      await loadStaffDetail(staffDetail);
    } catch {
      setStaffAdminError("Çalışma saatleri çakışıyor veya geçersiz.");
    } finally {
      setStaffSaving(false);
    }
  }, [loadStaffDetail, staffDetail, staffSaving, workingHours]);

  const saveTimeOff = useCallback(async () => {
    if (!staffDetail || staffSaving || !timeOffDate) return;
    const startMinute = timeOffFullDay ? null : timeToMinutes(timeOffStart);
    const endMinute = timeOffFullDay ? null : timeToMinutes(timeOffEnd);
    if (
      !timeOffFullDay &&
      (startMinute === null || endMinute === null || startMinute >= endMinute)
    ) {
      setStaffAdminError("İzin saat aralığını kontrol edin.");
      return;
    }
    setStaffSaving(true);
    setStaffAdminError("");
    const input = {
      localDate: timeOffDate,
      fullDay: timeOffFullDay,
      startMinute,
      endMinute,
    };
    try {
      if (editingTimeOffId)
        await updateStaffTimeOff(staffDetail.id, editingTimeOffId, input);
      else await addStaffTimeOff(staffDetail.id, input);
      await loadStaffDetail(staffDetail);
    } catch {
      setStaffAdminError("İzin kaydı çakışıyor veya geçersiz.");
    } finally {
      setStaffSaving(false);
    }
  }, [
    editingTimeOffId,
    loadStaffDetail,
    staffDetail,
    staffSaving,
    timeOffDate,
    timeOffEnd,
    timeOffFullDay,
    timeOffStart,
  ]);

  const saveBusinessProfile = useCallback(async () => {
    if (profileSaving) return;
    setProfileSaving(true);
    setProfileError("");
    setProfileNotice("");
    try {
      await updateBusinessProfile({
        businessName,
        phone: businessPhone.trim() || null,
        email: businessEmail.trim() || null,
        address: businessAddress.trim() || null,
        currencyCode: businessCurrency,
        themeKey: businessProfile?.themeKey ?? "default",
        logoData: businessProfile?.logoData ?? null,
        logoMimeType: businessProfile?.logoMimeType ?? null,
      });
      await loadBusinessProfile();
      setProfileNotice("İşletme bilgileri kaydedildi.");
    } catch {
      setProfileError(
        "İşletme bilgileri kaydedilemedi. Bilgileri kontrol edin.",
      );
    } finally {
      setProfileSaving(false);
    }
  }, [
    businessAddress,
    businessCurrency,
    businessEmail,
    businessName,
    businessPhone,
    businessProfile?.logoData,
    businessProfile?.logoMimeType,
    businessProfile?.themeKey,
    loadBusinessProfile,
    profileSaving,
  ]);

  const createBackup = useCallback(async () => {
    if (backupSaving) return;
    setBackupSaving(true);
    setBackupNotice("");
    setBackupError("");
    try {
      await createManualBackup();
      setBackupNotice("Yedek başarıyla oluşturuldu.");
    } catch {
      setBackupError("Yedek oluşturulamadı.");
    } finally {
      setBackupSaving(false);
    }
  }, [backupSaving]);

  const chooseRestoreCandidate = useCallback(async () => {
    setRestoreError("");
    try {
      const selected = await selectBackupFile();
      if (!selected) return;
      setRestoreCandidate(selected);
      setRestoreConfirming(true);
    } catch {
      setRestoreError("Yedek dosyası seçilemedi.");
    }
  }, []);

  const restoreErrorMessage = (error: unknown): string => {
    const message = error instanceof Error ? error.message : "";
    if (message.includes("SCHEMA_UNSUPPORTED"))
      return "Bu yedek daha yeni bir BeautySaloon sürümüyle oluşturulmuş.";
    if (
      message.includes("CANDIDATE_INVALID") ||
      message.includes("CANDIDATE_NOT_FOUND")
    )
      return "Seçilen dosya geçerli bir BeautySaloon yedeği değil.";
    if (message.includes("SAFETY_BACKUP"))
      return "Güvenlik yedeği oluşturulamadığı için geri yükleme başlatılmadı.";
    if (message.includes("ROLLBACK"))
      return "Geri yükleme tamamlanamadı. Mevcut verileriniz korunarak işlem geri alındı.";
    return "Geri yükleme tamamlanamadı. Mevcut verileriniz korunarak işlem geri alındı.";
  };

  const confirmRestore = useCallback(async () => {
    if (!restoreCandidate || restoreSaving) return;
    setRestoreSaving(true);
    setRestoreError("");
    setRestoreNotice("");
    try {
      await restoreDatabaseBackup(restoreCandidate);
      setAppointments([]);
      setCalendarAppointments([]);
      setCustomerList([]);
      setCustomerHistory(null);
      setAdminServices([]);
      setAdminStaff([]);
      await Promise.all([loadToday(), loadBusinessProfile()]);
      setRestoreNotice("Yedek başarıyla geri yüklendi.");
      setRestoreConfirming(false);
      setRestoreCandidate(null);
    } catch (error) {
      setRestoreError(restoreErrorMessage(error));
    } finally {
      setRestoreSaving(false);
    }
  }, [loadBusinessProfile, loadToday, restoreCandidate, restoreSaving]);

  const showDataFolder = useCallback(async () => {
    setDataFolderError("");
    try {
      await openDataFolder();
    } catch {
      setDataFolderError("Veri klasörü açılamadı.");
    }
  }, []);

  const connectGoogle = useCallback(async () => {
    if (googleConnecting) return;
    setGoogleConnecting(true);
    setGoogleError("");
    setGoogleNotice("");
    try {
      await connectGoogleCalendar();
      await loadGoogleConnection();
      setGoogleNotice("Google Takvim'e bağlanıldı.");
    } catch (error) {
      const message =
        error instanceof Error
          ? error.message
          : typeof error === "string"
            ? error
            : "";
      setGoogleError(
        message.includes("GOOGLE_CONFIG_MISSING")
          ? "Google Takvim bağlantısı bu bilgisayarda henüz yapılandırılmamış."
          : "Google bağlantısı tamamlanamadı.",
      );
    } finally {
      setGoogleConnecting(false);
    }
  }, [googleConnecting, loadGoogleConnection]);

  const changeAutomaticReminders = useCallback(
    async (enabled: boolean) => {
      if (reminderSettingSaving) return;
      setReminderSettingSaving(true);
      setReminderError("");
      try {
        const status = await setReminderAutomaticEnabled(enabled);
        setAutomaticRemindersEnabled(status.automaticEnabled);
        setReminderReadiness(status.state);
      } catch {
        setReminderError("Otomatik hatırlatma ayarı güncellenemedi.");
      } finally {
        setReminderSettingSaving(false);
      }
    },
    [reminderSettingSaving],
  );

  const disconnectGoogle = useCallback(async () => {
    if (googleDisconnecting) return;
    setGoogleDisconnecting(true);
    setGoogleError("");
    setGoogleNotice("");
    try {
      await disconnectGoogleCalendar();
      await loadGoogleConnection();
      setGoogleNotice("Google Takvim bağlantısı kesildi.");
    } catch {
      setGoogleError("Google bağlantısı kesilemedi.");
    } finally {
      setGoogleDisconnecting(false);
    }
  }, [googleDisconnecting, loadGoogleConnection]);

  const requestReminderCode = useCallback(async () => {
    if (reminderRequesting || !reminderEmail.trim()) {
      if (!reminderEmail.trim()) setReminderError("E-posta adresinizi girin.");
      return;
    }
    setReminderRequesting(true);
    setReminderError("");
    setReminderNotice("");
    try {
      await requestCloudOtp(reminderEmail.trim());
      setReminderCodeSent(true);
      setReminderNotice("Doğrulama kodu e-posta adresinize gönderildi.");
    } catch {
      setReminderError(
        "Kod gönderilemedi. E-posta adresini kontrol edip tekrar deneyin.",
      );
    } finally {
      setReminderRequesting(false);
    }
  }, [reminderEmail, reminderRequesting]);

  const verifyReminderCode = useCallback(async () => {
    if (reminderVerifying || !reminderEmail.trim() || !reminderCode.trim()) {
      setReminderError("E-posta ve doğrulama kodunu girin.");
      return;
    }
    setReminderVerifying(true);
    setReminderError("");
    setReminderNotice("");
    try {
      const status = await verifyCloudOtp(
        reminderEmail.trim(),
        reminderCode.trim(),
      );
      setReminderConnected(status.sessionPresent);
      setReminderCode("");
      setReminderCodeSent(false);
      setReminderNotice("WhatsApp Hatırlatmaları bağlandı.");
      await loadReminderConnection();
    } catch {
      setReminderError("Kod doğrulanamadı. Kodu kontrol edip tekrar deneyin.");
    } finally {
      setReminderVerifying(false);
    }
  }, [loadReminderConnection, reminderCode, reminderEmail, reminderVerifying]);

  const saveOnboardingBusiness = useCallback(async () => {
    if (onboardingSaving) return;
    if (!businessName.trim()) {
      setOnboardingError("İşletme adını girin.");
      return;
    }
    setOnboardingSaving(true);
    setOnboardingError("");
    try {
      const profile = await getBusinessProfile();
      const updated = await updateBusinessProfile({
        businessName: businessName.trim(),
        phone: profile.phone,
        email: profile.email,
        address: profile.address,
        currencyCode: profile.currencyCode,
        themeKey: profile.themeKey,
        logoData: profile.logoData,
        logoMimeType: profile.logoMimeType,
      });
      applyBusinessProfile(updated);
      setOnboardingStep(2);
    } catch {
      setOnboardingError("İşletme bilgileri kaydedilemedi.");
    } finally {
      setOnboardingSaving(false);
    }
  }, [applyBusinessProfile, businessName, onboardingSaving]);

  const saveOnboardingStaff = useCallback(async () => {
    if (onboardingSaving) return;
    if (!onboardingStaffName.trim()) {
      setOnboardingError("Personel adını girin.");
      return;
    }
    setOnboardingSaving(true);
    setOnboardingError("");
    try {
      const created = await createStaff({
        firstName: onboardingStaffName.trim(),
        lastName: null,
        phone: null,
        specialtyNote: null,
        colorKey: "sage",
        isActive: true,
      });
      setOnboardingStaffId(created.id);
      setOnboardingStep(3);
    } catch {
      setOnboardingError("Personel kaydedilemedi.");
    } finally {
      setOnboardingSaving(false);
    }
  }, [onboardingSaving, onboardingStaffName]);

  const saveOnboardingService = useCallback(async () => {
    if (onboardingSaving) return;
    const duration = Number(onboardingServiceDuration);
    const price = parseMinor(onboardingServicePrice);
    if (
      !onboardingServiceName.trim() ||
      !Number.isInteger(duration) ||
      duration <= 0 ||
      price === null
    ) {
      setOnboardingError("Hizmet adı, süre ve geçerli bir ücret girin.");
      return;
    }
    setOnboardingSaving(true);
    setOnboardingError("");
    try {
      const currentStaffId =
        onboardingStaffId || (await listActiveStaff())[0]?.id || "";
      if (!currentStaffId) throw new Error("staff missing");
      const categories = await listServiceCategories();
      const category =
        categories[0] ??
        (await createServiceCategory({ name: "Genel", isActive: true }));
      const service = await createService({
        categoryId: category.id,
        name: onboardingServiceName.trim(),
        durationMinutes: duration,
        defaultPriceMinor: price,
        isActive: true,
      });
      await persistStaffServices(currentStaffId, [service.id]);
      setOnboardingStaffId(currentStaffId);
      setOnboardingStep(4);
    } catch {
      setOnboardingError("Hizmet kaydedilemedi. Lütfen tekrar deneyin.");
    } finally {
      setOnboardingSaving(false);
    }
  }, [
    onboardingSaving,
    onboardingServiceDuration,
    onboardingServiceName,
    onboardingServicePrice,
    onboardingStaffId,
  ]);

  const finishOnboarding = useCallback(() => {
    setOnboardingOpen(false);
    setActiveItem("Bugün");
  }, []);

  if (onboardingLoading) {
    return (
      <main className="onboarding-loading">BeautySaloon hazırlanıyor...</main>
    );
  }

  if (onboardingOpen) {
    const stepLabels = ["İşletme", "Personel", "Hizmet", "Bağlantılar"];
    return (
      <main className="onboarding-shell" aria-label="Başlangıç kurulumu">
        <section className="onboarding-panel">
          <span className="brand-mark">BS</span>
          <div>
            <p className="onboarding-kicker">Başlangıç</p>
            <h1>BeautySaloon'u Hazırlayalım</h1>
            <p>Başlamak için birkaç temel bilgi yeterli.</p>
          </div>
          <ol className="onboarding-progress" aria-label="Kurulum adımları">
            {stepLabels.map((label, index) => (
              <li
                key={label}
                className={index + 1 === onboardingStep ? "is-current" : ""}
              >
                {label}
              </li>
            ))}
          </ol>
          {onboardingError && (
            <p className="form-error" role="alert">
              {onboardingError}
            </p>
          )}
          {onboardingStep === 1 && (
            <div className="onboarding-step">
              <h2>İşletme</h2>
              <label className="form-field">
                <span>İşletme adı</span>
                <input
                  aria-label="Onboarding işletme adı"
                  value={businessName}
                  disabled={onboardingSaving}
                  onChange={(event) => setBusinessName(event.target.value)}
                />
              </label>
              <button
                type="button"
                className="primary-action"
                disabled={onboardingSaving}
                onClick={() => void saveOnboardingBusiness()}
              >
                {onboardingSaving ? "Kaydediliyor..." : "Devam Et"}
              </button>
            </div>
          )}
          {onboardingStep === 2 && (
            <div className="onboarding-step">
              <h2>İlk Personel</h2>
              <label className="form-field">
                <span>Personel adı</span>
                <input
                  aria-label="Onboarding personel adı"
                  value={onboardingStaffName}
                  disabled={onboardingSaving}
                  onChange={(event) =>
                    setOnboardingStaffName(event.target.value)
                  }
                />
              </label>
              <div className="onboarding-actions">
                <button
                  type="button"
                  className="dismiss-button"
                  onClick={() => setOnboardingStep(1)}
                >
                  Geri
                </button>
                <button
                  type="button"
                  className="primary-action"
                  disabled={onboardingSaving}
                  onClick={() => void saveOnboardingStaff()}
                >
                  {onboardingSaving ? "Kaydediliyor..." : "Devam Et"}
                </button>
              </div>
            </div>
          )}
          {onboardingStep === 3 && (
            <div className="onboarding-step">
              <h2>İlk Hizmet</h2>
              <label className="form-field">
                <span>Hizmet adı</span>
                <input
                  aria-label="Onboarding hizmet adı"
                  value={onboardingServiceName}
                  disabled={onboardingSaving}
                  onChange={(event) =>
                    setOnboardingServiceName(event.target.value)
                  }
                />
              </label>
              <div className="booking-grid">
                <label className="form-field">
                  <span>Süre (dakika)</span>
                  <input
                    aria-label="Onboarding hizmet süresi"
                    type="number"
                    min="1"
                    value={onboardingServiceDuration}
                    disabled={onboardingSaving}
                    onChange={(event) =>
                      setOnboardingServiceDuration(event.target.value)
                    }
                  />
                </label>
                <label className="form-field">
                  <span>Varsayılan ücret ({businessCurrency})</span>
                  <input
                    aria-label="Onboarding hizmet ücreti"
                    inputMode="decimal"
                    value={onboardingServicePrice}
                    disabled={onboardingSaving}
                    onChange={(event) =>
                      setOnboardingServicePrice(event.target.value)
                    }
                  />
                </label>
              </div>
              <div className="onboarding-actions">
                <button
                  type="button"
                  className="dismiss-button"
                  onClick={() => setOnboardingStep(2)}
                >
                  Geri
                </button>
                <button
                  type="button"
                  className="primary-action"
                  disabled={onboardingSaving}
                  onClick={() => void saveOnboardingService()}
                >
                  {onboardingSaving ? "Kaydediliyor..." : "Devam Et"}
                </button>
              </div>
            </div>
          )}
          {onboardingStep === 4 && (
            <div className="onboarding-step">
              <h2>Bağlantılar</h2>
              <p>
                İsterseniz şimdi bağlayın, isterseniz daha sonra Ayarlar'dan
                ekleyin.
              </p>
              <div className="onboarding-connection">
                <strong>Google Takvim</strong>
                {googleConnected ? (
                  <span className="connection-state">Bağlı ✓</span>
                ) : (
                  <button
                    type="button"
                    className="dismiss-button"
                    disabled={googleConnecting}
                    onClick={() => void connectGoogle()}
                  >
                    {googleConnecting
                      ? "Bağlantı başlatılıyor..."
                      : "Google Takvim'e Bağlan"}
                  </button>
                )}
              </div>
              <div className="onboarding-connection">
                <strong>WhatsApp Hatırlatmaları</strong>
                {reminderConnected ? (
                  <span className="connection-state">Bağlı ✓</span>
                ) : (
                  <span>İsteğe bağlı</span>
                )}
              </div>
              {!reminderConnected && (
                <div className="reminder-form">
                  <label className="form-field">
                    <span>E-posta</span>
                    <input
                      aria-label="Onboarding hatırlatma e-postası"
                      type="email"
                      value={reminderEmail}
                      disabled={reminderRequesting || reminderVerifying}
                      onChange={(event) => setReminderEmail(event.target.value)}
                    />
                  </label>
                  <button
                    type="button"
                    className="dismiss-button"
                    disabled={reminderRequesting || reminderVerifying}
                    onClick={() => void requestReminderCode()}
                  >
                    {reminderRequesting ? "Kod gönderiliyor..." : "Kod Gönder"}
                  </button>
                  {reminderCodeSent && (
                    <>
                      <label className="form-field">
                        <span>Doğrulama kodu</span>
                        <input
                          aria-label="Onboarding doğrulama kodu"
                          inputMode="numeric"
                          value={reminderCode}
                          disabled={reminderVerifying}
                          onChange={(event) =>
                            setReminderCode(event.target.value)
                          }
                        />
                      </label>
                      <button
                        type="button"
                        className="primary-action"
                        disabled={reminderVerifying}
                        onClick={() => void verifyReminderCode()}
                      >
                        {reminderVerifying ? "Doğrulanıyor..." : "Doğrula"}
                      </button>
                    </>
                  )}
                </div>
              )}
              {(googleError || reminderError) && (
                <p className="form-error" role="alert">
                  {googleError || reminderError}
                </p>
              )}
              <div className="onboarding-actions">
                <button
                  type="button"
                  className="dismiss-button"
                  onClick={finishOnboarding}
                >
                  Şimdi Değil
                </button>
                <button
                  type="button"
                  className="primary-action"
                  onClick={finishOnboarding}
                >
                  Uygulamaya Geç
                </button>
              </div>
            </div>
          )}
        </section>
      </main>
    );
  }

  return (
    <div className="app-shell">
      <aside className="sidebar" aria-label="Ana gezinme">
        <div className="brand">
          <span className="brand-mark">BS</span>
          <div>
            <strong>BeautySaloon</strong>
            <span>Salon günlüğü</span>
          </div>
        </div>
        <nav className="nav-list">
          {navigationItems.map((item) => (
            <button
              key={item}
              type="button"
              className={
                item === activeItem ? "nav-item is-active" : "nav-item"
              }
              onClick={() => navigateToRoot(item)}
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
          <button
            type="button"
            className="primary-action"
            onClick={() =>
              void openBooking(
                activeItem === "Takvim" ? calendarDate : todayLocalDate(),
              )
            }
          >
            + Yeni Randevu
          </button>
        </header>

        {formNotice && (
          <p className="form-notice" role="status">
            {formNotice}
          </p>
        )}
        {pendingRootNavigation && (
          <section
            className="restore-confirmation"
            role="dialog"
            aria-modal="true"
            aria-labelledby="discard-booking-title"
          >
            <h2 id="discard-booking-title">Kaydedilmemiş bilgiler var.</h2>
            <p>Bu ekrandan çıkmak istiyor musunuz?</p>
            <div className="booking-actions">
              <button
                type="button"
                className="dismiss-button"
                onClick={() => setPendingRootNavigation(null)}
              >
                Kal
              </button>
              <button
                type="button"
                className="primary-action"
                onClick={() => {
                  const item = pendingRootNavigation;
                  setPendingRootNavigation(null);
                  completeRootNavigation(item);
                }}
              >
                Çık
              </button>
            </div>
          </section>
        )}
        {bookingOpen && (
          <section className="booking-surface" aria-labelledby="booking-title">
            <div className="booking-heading">
              <div>
                <h2 id="booking-title">Yeni Randevu</h2>
                <p>Müşteri, hizmet ve uygun zamanı seçin.</p>
              </div>
              <button
                type="button"
                className="dismiss-button"
                disabled={isSaving}
                onClick={() => {
                  resetBooking();
                  setBookingOpen(false);
                }}
              >
                Vazgeç
              </button>
            </div>
            {formError && (
              <p className="form-error" role="alert">
                {formError}
              </p>
            )}
            <div className="booking-grid">
              <label className="form-field">
                <span>Müşteri adı</span>
                <input
                  aria-label="Müşteri adı"
                  value={customerQuery}
                  disabled={Boolean(selectedCustomer) || isSaving}
                  onFocus={() => setCustomerPickerOpen(true)}
                  onChange={(event) => {
                    setCustomerQuery(event.target.value);
                    setCustomerPickerOpen(true);
                    setFormError("");
                  }}
                  placeholder="Ad Soyad"
                />
              </label>
              <label className="form-field">
                <span>Telefon (isteğe bağlı)</span>
                <input
                  aria-label="Telefon (isteğe bağlı)"
                  value={
                    selectedCustomer
                      ? customerPhoneLabel(selectedCustomer.phone)
                      : phone
                  }
                  disabled={Boolean(selectedCustomer) || isSaving}
                  onChange={(event) => {
                    setPhone(event.target.value);
                    if (!event.target.value.trim()) setWhatsappConsent(false);
                  }}
                  placeholder="05xx xxx xx xx"
                />
              </label>
              {!selectedCustomer && (
                <label className="archive-filter">
                  <input
                    type="checkbox"
                    checked={whatsappConsent}
                    disabled={isSaving || !phone.trim()}
                    onChange={(event) => setWhatsappConsent(event.target.checked)}
                  />{" "}
                  WhatsApp ile randevu hatırlatması gönderilebilir
                </label>
              )}
              {selectedCustomer && (
                <button
                  type="button"
                  className="text-action"
                  disabled={isSaving}
                  onClick={() => {
                    setSelectedCustomer(null);
                    setCustomerQuery("");
                    setPhone("");
                    setCustomerPickerOpen(false);
                  }}
                >
                  Farklı müşteri seç
                </button>
              )}
              {customerResults.length > 0 && (
                <ul className="customer-results" aria-label="Müşteri sonuçları">
                  {customerResults.map((customer) => (
                    <li key={customer.id}>
                      <button
                        type="button"
                        onClick={() => {
                          setSelectedCustomer(customer);
                          setCustomerQuery(customerName(customer));
                          setPhone(customer.phone ?? "");
                          setCustomerResults([]);
                          setCustomerPickerOpen(false);
                        }}
                      >
                        {customerName(customer)}
                        <span>{customerPhoneLabel(customer.phone)}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
              <fieldset className="service-picker">
                <legend>Hizmet</legend>
                {services.map((service) => (
                  <label className="service-option" key={service.id}>
                    <input
                      type="checkbox"
                      aria-label={service.name}
                      checked={serviceIds.includes(service.id)}
                      disabled={isSaving}
                      onChange={() =>
                        setServiceIds((current) =>
                          current.includes(service.id)
                            ? current.filter((id) => id !== service.id)
                            : [...current, service.id],
                        )
                      }
                    />
                    <span>{service.name}</span>
                    <small>{service.durationMinutes ?? 0} dk</small>
                  </label>
                ))}
              </fieldset>
              <label className="form-field">
                <span>Personel</span>
                <select
                  aria-label="Personel"
                  value={staffId}
                  disabled={isSaving}
                  onChange={(event) => setStaffId(event.target.value)}
                >
                  <option value="">Personel seçin</option>
                  {staff.map((item) => (
                    <option key={item.id} value={item.id}>
                      {staffName(item)}
                    </option>
                  ))}
                </select>
              </label>
              <label className="form-field">
                <span>Tarih</span>
                <input
                  aria-label="Tarih"
                  type="date"
                  value={localDate}
                  disabled={isSaving}
                  onChange={(event) => setLocalDate(event.target.value)}
                />
              </label>
              <label className="form-field">
                <span>Saat</span>
                <input
                  aria-label="Saat"
                  type="time"
                  value={localStartTime}
                  disabled={isSaving}
                  onChange={(event) => setLocalStartTime(event.target.value)}
                />
              </label>
            </div>
            <div className="booking-actions">
              <button
                type="button"
                className="dismiss-button"
                disabled={isSaving}
                onClick={() => {
                  resetBooking();
                  setBookingOpen(false);
                }}
              >
                Vazgeç
              </button>
              <button
                type="button"
                className="primary-action"
                disabled={isSaving}
                onClick={() => void saveAppointment()}
              >
                {isSaving ? "Kaydediliyor..." : "Randevuyu Kaydet"}
              </button>
            </div>
          </section>
        )}

        {activeItem === "Bugün" ? (
          <section className="today-timeline" aria-label="Bugünün randevuları">
            {todayError && (
              <div className="timeline-message" role="alert">
                <span>{todayError}</span>
                <button
                  type="button"
                  className="dismiss-button"
                  onClick={() => void loadToday()}
                >
                  Yeniden yükle
                </button>
              </div>
            )}
            {todayLoading ? (
              <p className="timeline-message">Randevular yükleniyor...</p>
            ) : appointments.length === 0 ? (
              <div className="today-empty-state">
                <span className="today-empty-icon">+</span>
                <h2>Bugün için randevu yok</h2>
                <p>Yeni bir randevu ekleyerek güne başlayın.</p>
              </div>
            ) : (
              appointments.map((appointment) => (
                <article className="appointment-card" key={appointment.id}>
                  <time>{timeInIstanbul(appointment.startAtUtc)}</time>
                  <div className="appointment-main">
                    <div className="appointment-title">
                      <h2>{appointment.customerName}</h2>
                      <span
                        className={`status-badge status-${appointment.status}`}
                      >
                        {statusLabel(appointment.status)}
                      </span>
                    </div>
                    <p>
                      {appointment.services
                        .map((service) => service.serviceNameSnapshot)
                        .join(", ")}
                    </p>
                    <p className="appointment-meta">
                      {appointment.staffName} ·{" "}
                      {appointment.totalDurationMinutes} dk
                    </p>
                    {appointment.status === "planned" && (
                      <div className="status-actions">
                        <button
                          type="button"
                          className="dismiss-button"
                          disabled={statusPendingId === appointment.id}
                          onClick={() =>
                            void changeStatus(appointment, "completed")
                          }
                        >
                          Tamamlandı
                        </button>
                        <button
                          type="button"
                          className="dismiss-button"
                          disabled={statusPendingId === appointment.id}
                          onClick={() =>
                            void changeStatus(appointment, "no_show")
                          }
                        >
                          Gelmedi
                        </button>
                        <button
                          type="button"
                          className="dismiss-button"
                          disabled={statusPendingId === appointment.id}
                          onClick={() =>
                            void changeStatus(appointment, "cancelled")
                          }
                        >
                          İptal
                        </button>
                      </div>
                    )}
                  </div>
                </article>
              ))
            )}
          </section>
        ) : activeItem === "Takvim" ? (
          <section className="calendar-surface" aria-label="Randevu takvimi">
            <div className="calendar-toolbar">
              <div
                className="calendar-mode"
                role="group"
                aria-label="Takvim görünümü"
              >
                <button
                  type="button"
                  className={
                    calendarMode === "day"
                      ? "mode-button is-active"
                      : "mode-button"
                  }
                  onClick={() => setCalendarMode("day")}
                >
                  Gün
                </button>
                <button
                  type="button"
                  className={
                    calendarMode === "week"
                      ? "mode-button is-active"
                      : "mode-button"
                  }
                  onClick={() => setCalendarMode("week")}
                >
                  Hafta
                </button>
              </div>
              <div className="calendar-navigation">
                <button
                  type="button"
                  className="dismiss-button"
                  aria-label="Önceki"
                  onClick={() =>
                    setCalendarDate((value) =>
                      addLocalDays(value, calendarMode === "week" ? -7 : -1),
                    )
                  }
                >
                  Önceki
                </button>
                <button
                  type="button"
                  className="dismiss-button"
                  onClick={() => {
                    const today = todayLocalDate();
                    if (calendarDate === today) void loadCalendar();
                    else setCalendarDate(today);
                  }}
                >
                  Bugün
                </button>
                <button
                  type="button"
                  className="dismiss-button"
                  aria-label="Sonraki"
                  onClick={() =>
                    setCalendarDate((value) =>
                      addLocalDays(value, calendarMode === "week" ? 7 : 1),
                    )
                  }
                >
                  Sonraki
                </button>
              </div>
            </div>
            <h2>
              {calendarMode === "week"
                ? `${dateLabel(calendarRange.startDate)} - ${dateLabel(calendarRange.endDate)}`
                : dateLabel(calendarDate)}
            </h2>
            {calendarError && (
              <div className="timeline-message" role="alert">
                <span>{calendarError}</span>
                <button
                  type="button"
                  className="dismiss-button"
                  onClick={() => void loadCalendar()}
                >
                  Yeniden yükle
                </button>
              </div>
            )}
            {calendarLoading ? (
              <p className="timeline-message">Takvim yükleniyor...</p>
            ) : calendarMode === "day" ? (
              <div className="calendar-day-list">
                {calendarAppointments.length === 0 ? (
                  <p className="calendar-empty">Randevu yok</p>
                ) : (
                  calendarAppointments.map((appointment) => (
                    <button
                      type="button"
                      className="calendar-appointment"
                      key={appointment.id}
                      onClick={() => void openAppointmentEditor(appointment)}
                    >
                      <time>{timeInIstanbul(appointment.startAtUtc)}</time>
                      <span>
                        <strong>{appointment.customerName}</strong>
                        <small>
                          {appointment.services
                            .map((service) => service.serviceNameSnapshot)
                            .join(", ")}{" "}
                          · {appointment.staffName} ·{" "}
                          {statusLabel(appointment.status)}
                        </small>
                      </span>
                    </button>
                  ))
                )}
              </div>
            ) : (
              <div className="calendar-week-grid">
                {Array.from({ length: 7 }, (_, index) =>
                  addLocalDays(calendarRange.startDate, index),
                ).map((date) => {
                  const items = calendarAppointments.filter(
                    (appointment) =>
                      localDateTime(appointment.startAtUtc)?.localDate === date,
                  );
                  return (
                    <section className="calendar-week-day" key={date}>
                      <h3>{dateLabel(date)}</h3>
                      {items.length === 0 ? (
                        <p>Randevu yok</p>
                      ) : (
                        items.map((appointment) => (
                          <button
                            type="button"
                            className="calendar-appointment"
                            key={appointment.id}
                            onClick={() =>
                              void openAppointmentEditor(appointment)
                            }
                          >
                            <time>
                              {timeInIstanbul(appointment.startAtUtc)}
                            </time>
                            <span>
                              <strong>{appointment.customerName}</strong>
                              <small>
                                {appointment.services
                                  .map((service) => service.serviceNameSnapshot)
                                  .join(", ")}{" "}
                                · {appointment.staffName} ·{" "}
                                {statusLabel(appointment.status)}
                              </small>
                            </span>
                          </button>
                        ))
                      )}
                    </section>
                  );
                })}
              </div>
            )}
          </section>
        ) : activeItem === "Müşteriler" ? (
          <section className="customers-surface" aria-label="Müşteriler">
            {customerHistory ? (
              <>
                <div className="customer-detail-heading">
                  <div>
                    <button
                      type="button"
                      className="text-action"
                      onClick={() => {
                        setCustomerHistory(null);
                        setHistoryError("");
                      }}
                    >
                      Müşteri listesine dön
                    </button>
                    <h2>{customerHistory.customer.name}</h2>
                    {customerHistory.customer.phone && (
                      <p>{customerPhoneLabel(customerHistory.customer.phone)}</p>
                    )}
                    <p className="customer-state">
                      {customerHistory.customer.isActive
                        ? "Aktif müşteri"
                        : "Arşivde"}
                    </p>
                  </div>
                  <div className="customer-detail-actions">
                    {customerHistory.customer.isActive ? (
                      <button
                        type="button"
                        className="primary-action"
                        onClick={() =>
                          void openBooking(todayLocalDate(), {
                            customer: customerForBooking(customerHistory),
                          })
                        }
                      >
                        + Yeni Randevu
                      </button>
                    ) : (
                      <p className="form-error" role="alert">
                        Yeni randevu için önce müşteriyi tekrar aktif edin.
                      </p>
                    )}
                    <button
                      type="button"
                      className="dismiss-button"
                      disabled={customerActionPending}
                      onClick={() => void updateCustomerArchiveState()}
                    >
                      {customerActionPending
                        ? "İşleniyor..."
                        : customerHistory.customer.isActive
                          ? "Arşivle"
                          : "Tekrar Aktif Et"}
                    </button>
                  </div>
                </div>
                <section
                  className="customer-history"
                  aria-labelledby="history-title"
                >
                  <h3 id="history-title">Randevu Geçmişi</h3>
                  {historyError && (
                    <div className="timeline-message" role="alert">
                      <span>{historyError}</span>
                      <button
                        type="button"
                        className="dismiss-button"
                        onClick={() =>
                          void loadCustomerHistory(
                            customerHistory.customer.customerId,
                          )
                        }
                      >
                        Yeniden yükle
                      </button>
                    </div>
                  )}
                  {historyLoading ? (
                    <p className="timeline-message">Geçmiş yükleniyor...</p>
                  ) : customerHistory.appointments.length === 0 ? (
                    <p className="calendar-empty">Henüz randevu geçmişi yok.</p>
                  ) : (
                    <div className="customer-history-list">
                      {customerHistory.appointments.map((appointment) => (
                        <article
                          className="history-item"
                          key={appointment.appointmentId}
                        >
                          <div>
                            <strong>
                              {appointment.localDate} · {appointment.localTime}
                            </strong>
                            <span
                              className={`status-badge status-${appointment.status}`}
                            >
                              {statusLabel(appointment.status)}
                            </span>
                          </div>
                          <p>
                            {appointment.services
                              .map((service) => service.serviceNameSnapshot)
                              .join(", ")}
                          </p>
                          <p className="appointment-meta">
                            {appointment.staffDisplayName}
                          </p>
                          <button
                            type="button"
                            className="dismiss-button"
                            onClick={() =>
                              void openRepeatBooking(appointment.appointmentId)
                            }
                          >
                            Tekrar Randevu Ver
                          </button>
                        </article>
                      ))}
                    </div>
                  )}
                  {historyHasMore && (
                    <button
                      type="button"
                      className="dismiss-button"
                      disabled={historyPageLoading}
                      onClick={() =>
                        void loadCustomerHistory(
                          customerHistory.customer.customerId,
                          historyOffset + customerHistory.appointments.length,
                          true,
                        )
                      }
                    >
                      {historyPageLoading
                        ? "Yükleniyor..."
                        : "Daha Fazla Göster"}
                    </button>
                  )}
                </section>
              </>
            ) : (
              <>
                <div className="customers-toolbar">
                  <label className="form-field">
                    <span>Müşteri ara</span>
                    <input
                      aria-label="Müşteri ara"
                      value={customerSearch}
                      onChange={(event) =>
                        setCustomerSearch(event.target.value)
                      }
                      placeholder="İsim veya telefon"
                    />
                  </label>
                  <label className="archive-filter">
                    <input
                      type="checkbox"
                      checked={showArchivedCustomers}
                      onChange={(event) =>
                        setShowArchivedCustomers(event.target.checked)
                      }
                    />{" "}
                    Arşivdekileri göster
                  </label>
                </div>
                {customersError && (
                  <div className="timeline-message" role="alert">
                    <span>{customersError}</span>
                    <button
                      type="button"
                      className="dismiss-button"
                      onClick={() => void loadCustomers()}
                    >
                      Yeniden yükle
                    </button>
                  </div>
                )}
                {customersLoading ? (
                  <p className="timeline-message">Müşteriler yükleniyor...</p>
                ) : customerList.length === 0 ? (
                  <p className="calendar-empty">Müşteri bulunamadı.</p>
                ) : (
                  <div className="customer-list">
                    {customerList.map((customer) => (
                      <button
                        type="button"
                        className="customer-row"
                        key={customer.id}
                        onClick={() => void loadCustomerHistory(customer.id)}
                      >
                        <span>
                          <strong>{customerName(customer)}</strong>
                          {customer.phone && (
                            <small>{customerPhoneLabel(customer.phone)}</small>
                          )}
                        </span>
                        {!customer.isActive && (
                          <span className="status-badge status-cancelled">
                            Arşivde
                          </span>
                        )}
                      </button>
                    ))}
                  </div>
                )}
              </>
            )}
          </section>
        ) : activeItem === "Hizmetler" ? (
          <section className="management-surface" aria-label="Hizmetler">
            <div className="management-heading">
              <div>
                <h2>Hizmetler</h2>
                <p>Yeni randevularda kullanılacak aktif hizmetleri yönetin.</p>
              </div>
              <label className="archive-filter">
                <input
                  type="checkbox"
                  checked={showInactiveServices}
                  onChange={(event) =>
                    setShowInactiveServices(event.target.checked)
                  }
                />{" "}
                Pasifleri göster
              </label>
            </div>
            <div
              className="calendar-mode"
              role="group"
              aria-label="Hizmetler görünümü"
            >
              <button
                type="button"
                className={
                  servicesView === "manage"
                    ? "mode-button is-active"
                    : "mode-button"
                }
                onClick={() => setServicesView("manage")}
              >
                Hizmetler
              </button>
              <button
                type="button"
                className={
                  servicesView === "statistics"
                    ? "mode-button is-active"
                    : "mode-button"
                }
                onClick={() => setServicesView("statistics")}
              >
                İstatistikler
              </button>
            </div>
            {servicesView === "manage" && (
              <>
                {servicesAdminError && (
                  <p className="form-error" role="alert">
                    {servicesAdminError}
                  </p>
                )}
                <section
                  className="management-form"
                  aria-labelledby="service-form-title"
                >
                  <div className="booking-heading">
                    <h3 id="service-form-title">
                      {editingService ? "Hizmeti Düzenle" : "Yeni Hizmet"}
                    </h3>
                    {editingService && (
                      <button
                        type="button"
                        className="text-action"
                        onClick={resetServiceForm}
                      >
                        Vazgeç
                      </button>
                    )}
                  </div>
                  <div className="booking-grid">
                    <label className="form-field">
                      <span>Hizmet adı</span>
                      <input
                        aria-label="Hizmet adı"
                        value={serviceName}
                        disabled={serviceSaving}
                        onChange={(event) => setServiceName(event.target.value)}
                      />
                    </label>
                    <label className="form-field">
                      <span>Kategori</span>
                      <select
                        aria-label="Hizmet kategorisi"
                        value={serviceCategoryId}
                        disabled={serviceSaving}
                        onChange={(event) =>
                          setServiceCategoryId(event.target.value)
                        }
                      >
                        <option value="">Kategori seçin</option>
                        {serviceCategories.map((category) => (
                          <option key={category.id} value={category.id}>
                            {category.name}
                          </option>
                        ))}
                      </select>
                    </label>
                    <label className="form-field">
                      <span>Süre (dakika)</span>
                      <input
                        aria-label="Hizmet süresi"
                        type="number"
                        min="1"
                        value={serviceDuration}
                        disabled={serviceSaving}
                        onChange={(event) =>
                          setServiceDuration(event.target.value)
                        }
                      />
                    </label>
                    <label className="form-field">
                      <span>Varsayılan fiyat ({currencyCode})</span>
                      <input
                        aria-label="Hizmet fiyatı"
                        inputMode="decimal"
                        value={servicePrice}
                        disabled={serviceSaving}
                        onChange={(event) =>
                          setServicePrice(event.target.value)
                        }
                      />
                    </label>
                  </div>
                  <div className="booking-actions">
                    <button
                      type="button"
                      className="primary-action"
                      disabled={serviceSaving}
                      onClick={() => void saveService()}
                    >
                      {serviceSaving
                        ? "Kaydediliyor..."
                        : editingService
                          ? "Hizmeti Güncelle"
                          : "Hizmet Ekle"}
                    </button>
                  </div>
                </section>
                {servicesAdminLoading ? (
                  <p className="timeline-message">Hizmetler yükleniyor...</p>
                ) : adminServices.length === 0 ? (
                  <p className="calendar-empty">Hizmet bulunamadı.</p>
                ) : (
                  <div className="management-list">
                    {adminServices.map((item) => (
                      <article className="management-row" key={item.id}>
                        <div>
                          <strong>{item.name}</strong>
                          <p>
                            {item.categoryName} · {item.durationMinutes ?? 0} dk
                            ·{" "}
                            {formatMinor(item.defaultPriceMinor, currencyCode)}
                          </p>
                        </div>
                        <div className="row-actions">
                          {!item.isActive && (
                            <span className="status-badge status-cancelled">
                              Pasif
                            </span>
                          )}
                          <button
                            type="button"
                            className="dismiss-button"
                            disabled={serviceSaving}
                            onClick={() => beginServiceEdit(item)}
                          >
                            Düzenle
                          </button>
                          <button
                            type="button"
                            className="dismiss-button"
                            disabled={serviceSaving}
                            onClick={() => void setServiceActive(item)}
                          >
                            {item.isActive ? "Pasife Al" : "Aktifleştir"}
                          </button>
                        </div>
                      </article>
                    ))}
                  </div>
                )}
              </>
            )}
            {servicesView === "statistics" && (
              <section
                className="statistics-surface"
                aria-label="Hizmet istatistikleri"
              >
                <div className="section-heading">
                  <div>
                    <h3>Hizmet İstatistikleri</h3>
                    <p>Tamamlanan işlemler ve ciro.</p>
                  </div>
                  <div
                    className="calendar-mode"
                    role="group"
                    aria-label="İstatistik tarihi"
                  >
                    <button
                      type="button"
                      className={
                        statisticsMode === "month"
                          ? "mode-button is-active"
                          : "mode-button"
                      }
                      onClick={() => setStatisticsMode("month")}
                    >
                      Aylık
                    </button>
                    <button
                      type="button"
                      className={
                        statisticsMode === "range"
                          ? "mode-button is-active"
                          : "mode-button"
                      }
                      onClick={() => setStatisticsMode("range")}
                    >
                      Tarih Aralığı
                    </button>
                  </div>
                </div>
                {statisticsMode === "month" ? (
                  <label className="form-field statistics-date-field">
                    <span>Ay</span>
                    <input
                      aria-label="İstatistik ayı"
                      type="month"
                      value={statisticsMonth}
                      onChange={(event) =>
                        setStatisticsMonth(event.target.value)
                      }
                    />
                  </label>
                ) : (
                  <div className="booking-grid">
                    <label className="form-field">
                      <span>Başlangıç</span>
                      <input
                        aria-label="İstatistik başlangıç tarihi"
                        type="date"
                        value={statisticsStartDate}
                        onChange={(event) =>
                          setStatisticsStartDate(event.target.value)
                        }
                      />
                    </label>
                    <label className="form-field">
                      <span>Bitiş</span>
                      <input
                        aria-label="İstatistik bitiş tarihi"
                        type="date"
                        value={statisticsEndDate}
                        onChange={(event) =>
                          setStatisticsEndDate(event.target.value)
                        }
                      />
                    </label>
                    <div className="statistics-filter-action">
                      <button
                        type="button"
                        className="primary-action"
                        disabled={statisticsLoading}
                        onClick={() =>
                          void loadServiceStatistics(
                            statisticsStartDate,
                            statisticsEndDate,
                          )
                        }
                      >
                        Göster
                      </button>
                    </div>
                  </div>
                )}
                {statisticsError && (
                  <div className="timeline-message" role="alert">
                    <span>{statisticsError}</span>
                    <button
                      type="button"
                      className="dismiss-button"
                      onClick={() => {
                        const range =
                          statisticsMode === "month"
                            ? monthRange(statisticsMonth)
                            : {
                                startDate: statisticsStartDate,
                                endDate: statisticsEndDate,
                              };
                        if (range)
                          void loadServiceStatistics(
                            range.startDate,
                            range.endDate,
                          );
                      }}
                    >
                      Yeniden yükle
                    </button>
                  </div>
                )}
                {statisticsLoading ? (
                  <p className="timeline-message">
                    İstatistikler yükleniyor...
                  </p>
                ) : serviceStatisticsRows.length === 0 && !statisticsError ? (
                  <p className="calendar-empty">
                    Seçilen dönemde tamamlanmış hizmet bulunmuyor.
                  </p>
                ) : serviceStatisticsRows.length > 0 ? (
                  <>
                    <div className="statistics-summary">
                      <span>
                        Toplam Tamamlanan İşlem:{" "}
                        <strong>
                          {serviceStatisticsRows.reduce(
                            (total, row) => total + row.completedCount,
                            0,
                          )}
                        </strong>
                      </span>
                      <span>
                        Toplam Ciro:{" "}
                        <strong>
                          {formatMinor(
                            serviceStatisticsRows.reduce(
                              (total, row) => total + row.revenueMinor,
                              0,
                            ),
                            currencyCode,
                          )}
                        </strong>
                      </span>
                    </div>
                    <div
                      className="statistics-table"
                      role="table"
                      aria-label="Hizmet ciro istatistikleri"
                    >
                      <div
                        className="statistics-table-row statistics-table-head"
                        role="row"
                      >
                        <span role="columnheader">Hizmet</span>
                        <span role="columnheader">Tamamlanan İşlem</span>
                        <span role="columnheader">Ciro</span>
                      </div>
                      {serviceStatisticsRows.map((row) => (
                        <div
                          className="statistics-table-row"
                          role="row"
                          key={`${row.serviceId}:${row.serviceNameSnapshot}`}
                        >
                          <span role="cell">{row.serviceNameSnapshot}</span>
                          <span role="cell">{row.completedCount}</span>
                          <strong role="cell">
                            {formatMinor(row.revenueMinor, currencyCode)}
                          </strong>
                        </div>
                      ))}
                    </div>
                  </>
                ) : null}
              </section>
            )}
          </section>
        ) : activeItem === "Personel" ? (
          <section className="management-surface" aria-label="Personel">
            <div className="management-heading">
              <div>
                <h2>Personel</h2>
                <p>Personel, sunduğu hizmetler ve çalışma uygunluğu.</p>
              </div>
              <label className="archive-filter">
                <input
                  type="checkbox"
                  checked={showInactiveStaff}
                  onChange={(event) =>
                    setShowInactiveStaff(event.target.checked)
                  }
                />{" "}
                Pasifleri göster
              </label>
            </div>
            {staffAdminError && (
              <p className="form-error" role="alert">
                {staffAdminError}
              </p>
            )}
            <section
              className="management-form"
              aria-labelledby="staff-form-title"
            >
              <div className="booking-heading">
                <h3 id="staff-form-title">
                  {editingStaff ? "Personeli Düzenle" : "Yeni Personel"}
                </h3>
                {editingStaff && (
                  <button
                    type="button"
                    className="text-action"
                    onClick={resetStaffForm}
                  >
                    Vazgeç
                  </button>
                )}
              </div>
              <div className="booking-grid">
                <label className="form-field">
                  <span>Ad</span>
                  <input
                    aria-label="Personel adı"
                    value={staffFirstName}
                    disabled={staffSaving}
                    onChange={(event) => setStaffFirstName(event.target.value)}
                  />
                </label>
                <label className="form-field">
                  <span>Soyad</span>
                  <input
                    aria-label="Personel soyadı"
                    value={staffLastName}
                    disabled={staffSaving}
                    onChange={(event) => setStaffLastName(event.target.value)}
                  />
                </label>
                <label className="form-field">
                  <span>Telefon (isteğe bağlı)</span>
                  <input
                    aria-label="Personel telefonu"
                    value={staffPhone}
                    disabled={staffSaving}
                    onChange={(event) => setStaffPhone(event.target.value)}
                  />
                </label>
                <label className="form-field">
                  <span>Uzmanlık notu</span>
                  <input
                    aria-label="Uzmanlık notu"
                    value={staffSpecialtyNote}
                    disabled={staffSaving}
                    onChange={(event) =>
                      setStaffSpecialtyNote(event.target.value)
                    }
                  />
                </label>
              </div>
              <div className="booking-actions">
                <button
                  type="button"
                  className="primary-action"
                  disabled={staffSaving}
                  onClick={() => void saveStaff()}
                >
                  {staffSaving
                    ? "Kaydediliyor..."
                    : editingStaff
                      ? "Personeli Güncelle"
                      : "Personel Ekle"}
                </button>
              </div>
            </section>
            {staffAdminLoading ? (
              <p className="timeline-message">Personel yükleniyor...</p>
            ) : adminStaff.length === 0 ? (
              <p className="calendar-empty">Personel bulunamadı.</p>
            ) : (
              <div className="management-list">
                {adminStaff.map((item) => (
                  <article className="management-row" key={item.id}>
                    <button
                      type="button"
                      className="management-main-button"
                      onClick={() => void loadStaffDetail(item)}
                    >
                      <strong>{staffName(item)}</strong>
                      <span>
                        {item.specialtyNote ?? "Personel detaylarını aç"}
                      </span>
                    </button>
                    <div className="row-actions">
                      {!item.isActive && (
                        <span className="status-badge status-cancelled">
                          Pasif
                        </span>
                      )}
                      <button
                        type="button"
                        className="dismiss-button"
                        disabled={staffSaving}
                        onClick={() => beginStaffEdit(item)}
                      >
                        Düzenle
                      </button>
                      <button
                        type="button"
                        className="dismiss-button"
                        disabled={staffSaving}
                        onClick={() => void setStaffActive(item)}
                      >
                        {item.isActive ? "Pasife Al" : "Aktifleştir"}
                      </button>
                    </div>
                  </article>
                ))}
              </div>
            )}
            {staffDetail && (
              <section
                className="staff-detail"
                aria-label={`${staffName(staffDetail)} ayarları`}
              >
                <div className="booking-heading">
                  <div>
                    <h3>{staffName(staffDetail)}</h3>
                    <p>Hizmet atamaları, çalışma saatleri ve izinler</p>
                  </div>
                  <button
                    type="button"
                    className="dismiss-button"
                    onClick={() => setStaffDetail(null)}
                  >
                    Kapat
                  </button>
                </div>
                {staffDetailLoading ? (
                  <p className="timeline-message">
                    Personel ayarları yükleniyor...
                  </p>
                ) : (
                  <>
                    <fieldset className="service-picker">
                      <legend>Sunduğu hizmetler</legend>
                      {staffServices.length === 0 ? (
                        <p>Aktif hizmet yok.</p>
                      ) : (
                        staffServices.map((item) => (
                          <label className="service-option" key={item.id}>
                            <input
                              type="checkbox"
                              aria-label={`${staffName(staffDetail)} ${item.name}`}
                              checked={staffServiceIds.includes(item.id)}
                              disabled={staffSaving}
                              onChange={() =>
                                setStaffServiceIds((current) =>
                                  current.includes(item.id)
                                    ? current.filter((id) => id !== item.id)
                                    : [...current, item.id],
                                )
                              }
                            />
                            <span>{item.name}</span>
                            <small>{item.durationMinutes ?? 0} dk</small>
                          </label>
                        ))
                      )}
                      <button
                        type="button"
                        className="dismiss-button"
                        disabled={staffSaving}
                        onClick={() => void saveStaffServices()}
                      >
                        Hizmet Atamalarını Kaydet
                      </button>
                    </fieldset>
                    <section className="schedule-section">
                      <div className="section-heading">
                        <div>
                          <h3>Haftalık çalışma saatleri</h3>
                          <p>
                            {scheduleUnrestricted
                              ? "Henüz kısıt tanımlanmadı; personel haftalık saatler açısından serbest kabul edilir."
                              : "Tanımlı saatlerin dışındaki zamanlar uygun değildir."}
                          </p>
                        </div>
                        <button
                          type="button"
                          className="dismiss-button"
                          disabled={staffSaving}
                          onClick={() =>
                            setWorkingHours((current) => [
                              ...current,
                              {
                                staffId: staffDetail.id,
                                weekday: 0,
                                startMinute: 540,
                                endMinute: 1020,
                              },
                            ])
                          }
                        >
                          Aralık Ekle
                        </button>
                      </div>
                      <div className="interval-list">
                        {workingHours.length === 0 ? (
                          <p className="calendar-empty">
                            Çalışma saati tanımlanmamış.
                          </p>
                        ) : (
                          workingHours.map((interval, index) => (
                            <div
                              className="interval-row"
                              key={`${interval.weekday}-${index}`}
                            >
                              <select
                                aria-label={`Çalışma günü ${index + 1}`}
                                value={interval.weekday}
                                disabled={staffSaving}
                                onChange={(event) =>
                                  setWorkingHours((current) =>
                                    current.map((item, itemIndex) =>
                                      itemIndex === index
                                        ? {
                                            ...item,
                                            weekday: Number(event.target.value),
                                          }
                                        : item,
                                    ),
                                  )
                                }
                              >
                                {weekdays.map((day, weekday) => (
                                  <option key={day} value={weekday}>
                                    {day}
                                  </option>
                                ))}
                              </select>
                              <input
                                aria-label={`Başlangıç saati ${index + 1}`}
                                type="time"
                                value={minutesToTime(interval.startMinute)}
                                disabled={staffSaving}
                                onChange={(event) => {
                                  const minutes = timeToMinutes(
                                    event.target.value,
                                  );
                                  if (minutes !== null)
                                    setWorkingHours((current) =>
                                      current.map((item, itemIndex) =>
                                        itemIndex === index
                                          ? { ...item, startMinute: minutes }
                                          : item,
                                      ),
                                    );
                                }}
                              />
                              <input
                                aria-label={`Bitiş saati ${index + 1}`}
                                type="time"
                                value={minutesToTime(interval.endMinute)}
                                disabled={staffSaving}
                                onChange={(event) => {
                                  const minutes = timeToMinutes(
                                    event.target.value,
                                  );
                                  if (minutes !== null)
                                    setWorkingHours((current) =>
                                      current.map((item, itemIndex) =>
                                        itemIndex === index
                                          ? { ...item, endMinute: minutes }
                                          : item,
                                      ),
                                    );
                                }}
                              />
                              <button
                                type="button"
                                className="text-action"
                                disabled={staffSaving}
                                onClick={() =>
                                  setWorkingHours((current) =>
                                    current.filter(
                                      (_, itemIndex) => itemIndex !== index,
                                    ),
                                  )
                                }
                              >
                                Kaldır
                              </button>
                            </div>
                          ))
                        )}
                      </div>
                      <button
                        type="button"
                        className="dismiss-button"
                        disabled={staffSaving}
                        onClick={() => void saveWorkingHours()}
                      >
                        Çalışma Saatlerini Kaydet
                      </button>
                    </section>
                    <section className="schedule-section">
                      <div className="section-heading">
                        <div>
                          <h3>İzin ve kapalı günler</h3>
                          <p>Tam gün veya saat aralığıyla izin tanımlayın.</p>
                        </div>
                      </div>
                      <div className="time-off-form">
                        <label className="form-field">
                          <span>Tarih</span>
                          <input
                            aria-label="İzin tarihi"
                            type="date"
                            value={timeOffDate}
                            disabled={staffSaving}
                            onChange={(event) =>
                              setTimeOffDate(event.target.value)
                            }
                          />
                        </label>
                        <label className="archive-filter">
                          <input
                            aria-label="Tam gün izin"
                            type="checkbox"
                            checked={timeOffFullDay}
                            disabled={staffSaving}
                            onChange={(event) =>
                              setTimeOffFullDay(event.target.checked)
                            }
                          />{" "}
                          Tam gün
                        </label>
                        {!timeOffFullDay && (
                          <>
                            <label className="form-field">
                              <span>Başlangıç</span>
                              <input
                                aria-label="İzin başlangıç"
                                type="time"
                                value={timeOffStart}
                                disabled={staffSaving}
                                onChange={(event) =>
                                  setTimeOffStart(event.target.value)
                                }
                              />
                            </label>
                            <label className="form-field">
                              <span>Bitiş</span>
                              <input
                                aria-label="İzin bitiş"
                                type="time"
                                value={timeOffEnd}
                                disabled={staffSaving}
                                onChange={(event) =>
                                  setTimeOffEnd(event.target.value)
                                }
                              />
                            </label>
                          </>
                        )}
                        <button
                          type="button"
                          className="dismiss-button"
                          disabled={staffSaving}
                          onClick={() => void saveTimeOff()}
                        >
                          {editingTimeOffId ? "İzni Güncelle" : "İzin Ekle"}
                        </button>
                      </div>
                      <div className="time-off-list">
                        {timeOffItems.length === 0 ? (
                          <p className="calendar-empty">
                            Yaklaşan izin kaydı yok.
                          </p>
                        ) : (
                          timeOffItems.map((item) => (
                            <div className="management-row" key={item.id}>
                              <div>
                                <strong>{item.localDate}</strong>
                                <p>
                                  {item.fullDay
                                    ? "Tam gün"
                                    : `${minutesToTime(item.startMinute ?? 0)} - ${minutesToTime(item.endMinute ?? 0)}`}
                                </p>
                              </div>
                              <div className="row-actions">
                                <button
                                  type="button"
                                  className="dismiss-button"
                                  disabled={staffSaving}
                                  onClick={() => {
                                    setEditingTimeOffId(item.id);
                                    setTimeOffDate(item.localDate);
                                    setTimeOffFullDay(item.fullDay);
                                    setTimeOffStart(
                                      minutesToTime(item.startMinute ?? 540),
                                    );
                                    setTimeOffEnd(
                                      minutesToTime(item.endMinute ?? 600),
                                    );
                                  }}
                                >
                                  Düzenle
                                </button>
                                <button
                                  type="button"
                                  className="text-action"
                                  disabled={staffSaving}
                                  onClick={async () => {
                                    try {
                                      await removeStaffTimeOff(
                                        staffDetail.id,
                                        item.id,
                                      );
                                      await loadStaffDetail(staffDetail);
                                    } catch {
                                      setStaffAdminError(
                                        "İzin kaydı silinemedi.",
                                      );
                                    }
                                  }}
                                >
                                  Kaldır
                                </button>
                              </div>
                            </div>
                          ))
                        )}
                      </div>
                    </section>
                  </>
                )}
              </section>
            )}
          </section>
        ) : activeItem === "Ayarlar" ? (
          <section className="settings-foundation" aria-label="Ayarlar">
            <section
              className="settings-section settings-section--wide"
              aria-labelledby="business-profile-title"
            >
              <div>
                <h2 id="business-profile-title">İşletme</h2>
                <p>Salonunuzun temel bilgilerini güncelleyin.</p>
              </div>
              {profileError && (
                <p className="form-error" role="alert">
                  {profileError}
                </p>
              )}
              {profileNotice && (
                <p className="form-notice" role="status">
                  {profileNotice}
                </p>
              )}
              {profileLoading ? (
                <p className="timeline-message">
                  İşletme bilgileri yükleniyor...
                </p>
              ) : (
                <>
                  <div className="booking-grid">
                    <label className="form-field">
                      <span>İşletme adı</span>
                      <input
                        aria-label="İşletme adı"
                        value={businessName}
                        disabled={profileSaving}
                        onChange={(event) =>
                          setBusinessName(event.target.value)
                        }
                      />
                    </label>
                    <label className="form-field">
                      <span>Telefon (isteğe bağlı)</span>
                      <input
                        aria-label="İşletme telefonu"
                        value={businessPhone}
                        disabled={profileSaving}
                        onChange={(event) =>
                          setBusinessPhone(event.target.value)
                        }
                      />
                    </label>
                    <label className="form-field">
                      <span>E-posta (isteğe bağlı)</span>
                      <input
                        aria-label="İşletme e-postası"
                        type="email"
                        value={businessEmail}
                        disabled={profileSaving}
                        onChange={(event) =>
                          setBusinessEmail(event.target.value)
                        }
                      />
                    </label>
                    <label className="form-field">
                      <span>Para birimi</span>
                      <select
                        aria-label="Para birimi"
                        value={businessCurrency}
                        disabled={profileSaving}
                        onChange={(event) =>
                          setBusinessCurrency(event.target.value)
                        }
                      >
                        <option value="TRY">Türk Lirası (TRY)</option>
                      </select>
                    </label>
                    <label className="form-field form-field--wide">
                      <span>Adres (isteğe bağlı)</span>
                      <input
                        aria-label="İşletme adresi"
                        value={businessAddress}
                        disabled={profileSaving}
                        onChange={(event) =>
                          setBusinessAddress(event.target.value)
                        }
                      />
                    </label>
                  </div>
                  <div className="settings-summary">
                    <span>Tema: Varsayılan</span>
                    <span>
                      Logo:{" "}
                      {businessProfile?.logoMimeType
                        ? "Tanımlı"
                        : "Henüz eklenmedi"}
                    </span>
                  </div>
                  <button
                    type="button"
                    className="primary-action"
                    disabled={profileSaving}
                    onClick={() => void saveBusinessProfile()}
                  >
                    {profileSaving
                      ? "Kaydediliyor..."
                      : "İşletme Bilgilerini Kaydet"}
                  </button>
                </>
              )}
            </section>
            <section
              className="settings-section settings-section--wide"
              aria-labelledby="calendar-settings-title"
            >
              <div>
                <h2 id="calendar-settings-title">Google Takvim</h2>
                <p>Randevularınızı Google Takvim ile eşitleyin.</p>
              </div>
              {googleError && (
                <p className="form-error" role="alert">
                  {googleError}
                </p>
              )}
              {googleNotice && (
                <p className="form-notice" role="status">
                  {googleNotice}
                </p>
              )}
              {googleLoading ? (
                <p className="timeline-message">
                  Google Takvim durumu yükleniyor...
                </p>
              ) : googleConnected ? (
                <div className="settings-actions">
                  <p className="connection-state">Google Takvim — Bağlı ✓</p>
                  <button
                    type="button"
                    className="dismiss-button"
                    disabled={googleDisconnecting}
                    onClick={() => void disconnectGoogle()}
                  >
                    {googleDisconnecting
                      ? "Bağlantı kesiliyor..."
                      : "Bağlantıyı Kes"}
                  </button>
                </div>
              ) : (
                <div className="settings-actions">
                  <p className="connection-state">Google Takvim bağlı değil.</p>
                  <button
                    type="button"
                    className="primary-action"
                    disabled={googleConnecting}
                    onClick={() => void connectGoogle()}
                  >
                    {googleConnecting
                      ? "Bağlantı başlatılıyor..."
                      : "Google Takvim'e Bağlan"}
                  </button>
                </div>
              )}
            </section>
            <section
              className="settings-section settings-section--wide"
              aria-labelledby="reminder-settings-title"
            >
              <div>
                <h2 id="reminder-settings-title">WhatsApp Hatırlatmaları</h2>
                <p>Randevu hatırlatmalarınız için bağlantınızı doğrulayın.</p>
              </div>
              {reminderError && (
                <p className="form-error" role="alert">
                  {reminderError}
                </p>
              )}
              {reminderNotice && (
                <p className="form-notice" role="status">
                  {reminderNotice}
                </p>
              )}
              {reminderLoading ? (
                <p className="timeline-message">
                  Hatırlatma durumu yükleniyor...
                </p>
              ) : reminderConnected ? (
                <div className="settings-actions">
                  <p className="connection-state">
                    {reminderConnectionUnavailable
                      ? "Bağlantı durumu şu anda doğrulanamıyor"
                      : reminderReadiness === "ready"
                      ? "Hazır / Hatırlatmalar aktif"
                      : "Bağlı, ancak hatırlatmalar hazır değil"}
                  </p>
                  <label className="archive-filter">
                    <input
                      type="checkbox"
                      checked={automaticRemindersEnabled}
                      disabled={reminderSettingSaving || reminderConnectionUnavailable}
                      onChange={(event) =>
                        void changeAutomaticReminders(event.target.checked)
                      }
                    />{" "}
                    Otomatik WhatsApp hatırlatmaları
                  </label>
                </div>
              ) : (
                <div className="reminder-form">
                  <label className="form-field">
                    <span>E-posta</span>
                    <input
                      aria-label="Hatırlatma e-postası"
                      type="email"
                      value={reminderEmail}
                      disabled={reminderRequesting || reminderVerifying}
                      onChange={(event) => setReminderEmail(event.target.value)}
                    />
                  </label>
                  <button
                    type="button"
                    className="dismiss-button"
                    disabled={reminderRequesting || reminderVerifying}
                    onClick={() => void requestReminderCode()}
                  >
                    {reminderRequesting ? "Kod gönderiliyor..." : "Kod Gönder"}
                  </button>
                  {reminderCodeSent && (
                    <>
                      <label className="form-field">
                        <span>Doğrulama kodu</span>
                        <input
                          aria-label="Doğrulama kodu"
                          inputMode="numeric"
                          value={reminderCode}
                          disabled={reminderVerifying}
                          onChange={(event) =>
                            setReminderCode(event.target.value)
                          }
                        />
                      </label>
                      <button
                        type="button"
                        className="primary-action"
                        disabled={reminderVerifying}
                        onClick={() => void verifyReminderCode()}
                      >
                        {reminderVerifying ? "Doğrulanıyor..." : "Doğrula"}
                      </button>
                    </>
                  )}
                </div>
              )}
            </section>
            <section
              className="settings-section settings-section--wide"
              aria-labelledby="backup-title"
            >
              <div>
                <h2 id="backup-title">Veri ve Yedekleme</h2>
                <p>
                  Verilerinizi yedekleyin veya daha önce oluşturduğunuz bir
                  yedeği geri yükleyin.
                </p>
              </div>
              {backupNotice && (
                <p className="form-notice" role="status">
                  {backupNotice}
                </p>
              )}
              {backupError && (
                <p className="form-error" role="alert">
                  {backupError}
                </p>
              )}
              {restoreNotice && (
                <p className="form-notice" role="status">
                  {restoreNotice}
                </p>
              )}
              {restoreError && (
                <p className="form-error" role="alert">
                  {restoreError}
                </p>
              )}
              {dataFolderError && (
                <p className="form-error" role="alert">
                  {dataFolderError}
                </p>
              )}
              <div className="settings-actions">
                <button
                  type="button"
                  className="primary-action"
                  disabled={backupSaving}
                  onClick={() => void createBackup()}
                >
                  {backupSaving ? "Yedek oluşturuluyor..." : "Yedek Oluştur"}
                </button>
                <button
                  type="button"
                  className="dismiss-button"
                  disabled={restoreSaving}
                  onClick={() => void chooseRestoreCandidate()}
                >
                  Yedekten Geri Yükle
                </button>
                <button
                  type="button"
                  className="dismiss-button"
                  onClick={() => void showDataFolder()}
                >
                  Veri Klasörünü Aç
                </button>
              </div>
              {restoreConfirming && (
                <div
                  className="restore-confirmation"
                  role="dialog"
                  aria-modal="true"
                  aria-labelledby="restore-confirmation-title"
                >
                  <h3 id="restore-confirmation-title">Yedeği geri yükle</h3>
                  <p>
                    Bu işlem mevcut BeautySaloon verilerinin yerine seçtiğiniz
                    yedeği geri yükleyecek. İşlemden önce mevcut verileriniz
                    için otomatik güvenlik yedeği oluşturulur.
                  </p>
                  <div className="booking-actions">
                    <button
                      type="button"
                      className="dismiss-button"
                      disabled={restoreSaving}
                      onClick={() => {
                        setRestoreConfirming(false);
                        setRestoreCandidate(null);
                      }}
                    >
                      Vazgeç
                    </button>
                    <button
                      type="button"
                      className="primary-action"
                      disabled={restoreSaving}
                      onClick={() => void confirmRestore()}
                    >
                      {restoreSaving ? "Geri yükleniyor..." : "Geri Yükle"}
                    </button>
                  </div>
                </div>
              )}
            </section>
          </section>
        ) : (
          <section className="screen-placeholder">
            <h2>{activeItem}</h2>
            <p>Bu bölüm sonraki adımda tamamlanacak.</p>
          </section>
        )}

        {editingAppointment && (
          <section
            className="booking-surface appointment-editor"
            aria-labelledby="edit-title"
          >
            <div className="booking-heading">
              <div>
                <h2 id="edit-title">Randevuyu Düzenle</h2>
                <p>
                  {editingAppointment.customerName} ·{" "}
                  {editingAppointment.staffName} ·{" "}
                  {statusLabel(editingAppointment.status)}
                </p>
              </div>
              <button
                type="button"
                className="dismiss-button"
                disabled={isEditSaving}
                onClick={resetAppointmentEditor}
              >
                Vazgeç
              </button>
            </div>
            <p className="selection-note">
              Kaynak hizmetler:{" "}
              {editingAppointment.services
                .map((service) => service.serviceNameSnapshot)
                .join(", ")}
            </p>
            {editError && (
              <p className="form-error" role="alert">
                {editError}
              </p>
            )}
            <div className="booking-grid">
              <fieldset className="service-picker">
                <legend>Hizmet</legend>
                {editServices.map((service) => (
                  <label className="service-option" key={service.id}>
                    <input
                      type="checkbox"
                      aria-label={`Düzenle ${service.name}`}
                      checked={editServiceIds.includes(service.id)}
                      disabled={isEditSaving}
                      onChange={() =>
                        setEditServiceIds((current) =>
                          current.includes(service.id)
                            ? current.filter((id) => id !== service.id)
                            : [...current, service.id],
                        )
                      }
                    />
                    <span>{service.name}</span>
                    <small>{service.durationMinutes ?? 0} dk</small>
                  </label>
                ))}
              </fieldset>
              <label className="form-field">
                <span>Personel</span>
                <select
                  aria-label="Düzenle personel"
                  value={editStaffId}
                  disabled={isEditSaving}
                  onChange={(event) => setEditStaffId(event.target.value)}
                >
                  <option value="">Personel seçin</option>
                  {editStaff.map((item) => (
                    <option key={item.id} value={item.id}>
                      {staffName(item)}
                    </option>
                  ))}
                </select>
              </label>
              <label className="form-field">
                <span>Tarih</span>
                <input
                  aria-label="Düzenle tarih"
                  type="date"
                  value={editDate}
                  disabled={isEditSaving}
                  onChange={(event) => setEditDate(event.target.value)}
                />
              </label>
              <label className="form-field">
                <span>Saat</span>
                <input
                  aria-label="Düzenle saat"
                  type="time"
                  value={editTime}
                  disabled={isEditSaving}
                  onChange={(event) => setEditTime(event.target.value)}
                />
              </label>
            </div>
            <div className="booking-actions">
              <button
                type="button"
                className="dismiss-button"
                disabled={isEditSaving}
                onClick={resetAppointmentEditor}
              >
                Vazgeç
              </button>
              <button
                type="button"
                className="primary-action"
                disabled={isEditSaving}
                onClick={() => void saveAppointmentEdit()}
              >
                {isEditSaving ? "Kaydediliyor..." : "Değişiklikleri Kaydet"}
              </button>
            </div>
          </section>
        )}
      </main>
    </div>
  );
}
