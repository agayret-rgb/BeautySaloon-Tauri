import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";

const api = vi.hoisted(() => ({
  createAppointment: vi.fn(),
  createCustomer: vi.fn(),
  createServiceCategory: vi.fn(),
  updateServiceCategory: vi.fn(),
  getCustomerHistory: vi.fn(),
  getRepeatBookingSeed: vi.fn(),
  listActiveServices: vi.fn(),
  listActiveStaff: vi.fn(),
  listAppointmentsByDate: vi.fn(),
  listAppointmentsByDateRange: vi.fn(),
  archiveCustomer: vi.fn(),
  getBusinessProfile: vi.fn(),
  getOnboardingState: vi.fn().mockResolvedValue({
    needsOnboarding: false,
    nextStep: 4,
  }),
  updateBusinessProfile: vi.fn(),
  createManualBackup: vi.fn(),
  restoreDatabaseBackup: vi.fn(),
  openDataFolder: vi.fn(),
  selectBackupFile: vi.fn(),
  listServices: vi.fn(),
  listServiceCategories: vi.fn(),
  createService: vi.fn(),
  serviceStatistics: vi.fn(),
  updateService: vi.fn(),
  deactivateService: vi.fn(),
  reactivateService: vi.fn(),
  listStaff: vi.fn(),
  createStaff: vi.fn(),
  updateStaff: vi.fn(),
  deactivateStaff: vi.fn(),
  reactivateStaff: vi.fn(),
  listStaffServiceIds: vi.fn(),
  setStaffServices: vi.fn(),
  listStaffWorkingHours: vi.fn(),
  setStaffWorkingHours: vi.fn(),
  isStaffScheduleUnrestricted: vi.fn(),
  listStaffTimeOff: vi.fn(),
  addStaffTimeOff: vi.fn(),
  updateStaffTimeOff: vi.fn(),
  removeStaffTimeOff: vi.fn(),
  getGoogleCalendarStatus: vi.fn(),
  connectGoogleCalendar: vi.fn(),
  disconnectGoogleCalendar: vi.fn(),
  getCloudConnectionStatus: vi.fn(),
  getReminderReadiness: vi.fn(),
  requestCloudOtp: vi.fn(),
  verifyCloudOtp: vi.fn(),
  setReminderAutomaticEnabled: vi.fn(),
  reactivateCustomer: vi.fn(),
  searchCustomers: vi.fn(),
  searchCustomersByStatus: vi.fn(),
  updateCustomer: vi.fn(),
  updateAppointment: vi.fn(),
}));

vi.mock("./tauriApi", () => api);

const activeService = {
  id: "service-1",
  name: "Saç Kesimi",
  durationMinutes: 45,
  defaultPriceMinor: 15000,
  isActive: true,
};
const secondActiveService = {
  id: "service-3",
  name: "Fön",
  durationMinutes: 30,
  defaultPriceMinor: 9000,
  isActive: true,
};
const inactiveService = {
  id: "service-2",
  name: "Eski Hizmet",
  durationMinutes: 30,
  defaultPriceMinor: 0,
  isActive: false,
};
const activeStaff = {
  id: "staff-1",
  firstName: "Ece",
  lastName: "Demir",
  isActive: true,
};
const inactiveStaff = {
  id: "staff-2",
  firstName: "Eski",
  lastName: "Personel",
  isActive: false,
};
const existingCustomer = {
  id: "customer-1",
  firstName: "Ayşe",
  lastName: "Yılmaz",
  phone: "+905551112233",
  email: null,
  notes: "Eski not",
  whatsappReminderEnabled: true,
  whatsappConsentConfirmed: false,
  isActive: true,
};
const todayAppointment = {
  id: "appointment-1",
  customerId: "customer-1",
  staffId: "staff-1",
  startAtUtc: "2026-09-01T07:00:00.000Z",
  endAtUtc: "2026-09-01T07:45:00.000Z",
  totalDurationMinutes: 45,
  status: "planned",
  note: null,
  whatsappReminderEnabled: true,
  whatsappReminderEffective: true,
  customerName: "Ayşe Yılmaz",
  customerPhone: null,
  staffName: "Ece Demir",
  serviceNames: ["Geçmiş Saç Kesimi"],
  services: [
    {
      serviceId: "service-1",
      serviceNameSnapshot: "Geçmiş Saç Kesimi",
      durationMinutesSnapshot: 45,
      listedPriceSnapshotMinor: 15000,
      chargedPriceMinor: 15000,
      sortOrder: 10,
    },
  ],
  updatedAt: "2026-09-01T07:00:00.000Z",
};
const customerHistory = {
  customer: {
    customerId: "customer-1",
    name: "Ayşe Yılmaz",
    phone: "+905551112233",
    isActive: true,
  },
  appointments: [
    {
      appointmentId: "history-1",
      localDate: "2026-08-30",
      localTime: "14:00",
      status: "completed",
      staffId: "staff-1",
      staffDisplayName: "Ece Demir",
      services: [
        {
          serviceId: "service-1",
          serviceNameSnapshot: "Geçmiş Saç Kesimi",
          durationMinutesSnapshot: 45,
          listedPriceSnapshotMinor: 15000,
          chargedPriceMinor: 12500,
          sortOrder: 10,
        },
        {
          serviceId: "service-3",
          serviceNameSnapshot: "Geçmiş Fön",
          durationMinutesSnapshot: 30,
          listedPriceSnapshotMinor: 9000,
          chargedPriceMinor: 9000,
          sortOrder: 20,
        },
      ],
      totalChargedMinor: 21500,
    },
  ],
  limit: 10,
  offset: 0,
};

describe("New appointment flow", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.listActiveServices.mockResolvedValue([
      activeService,
      secondActiveService,
      inactiveService,
    ]);
    api.listActiveStaff.mockResolvedValue([activeStaff, inactiveStaff]);
    api.listServices.mockResolvedValue([activeService]);
    api.listServiceCategories.mockResolvedValue([]);
    api.listStaff.mockResolvedValue([activeStaff]);
    api.searchCustomers.mockResolvedValue([existingCustomer]);
    api.getReminderReadiness.mockResolvedValue({
      state: "disconnected",
      automaticEnabled: false,
    });
    api.createCustomer.mockResolvedValue({
      id: "customer-new",
      firstName: "Deniz",
      lastName: "Kaya",
      phone: null,
      isActive: true,
    });
    api.createAppointment.mockResolvedValue({ id: "appointment-1" });
    api.listAppointmentsByDate.mockResolvedValue([]);
    api.listAppointmentsByDateRange.mockResolvedValue([]);
    api.updateAppointment.mockResolvedValue(todayAppointment);
    api.searchCustomersByStatus.mockResolvedValue([existingCustomer]);
    api.getCustomerHistory.mockResolvedValue(customerHistory);
    api.getRepeatBookingSeed.mockResolvedValue({
      customerId: "customer-1",
      customerDisplayName: "Ayşe Yılmaz",
      staffId: "staff-1",
      services: [
        {
          serviceId: "service-1",
          serviceNameSnapshot: "Geçmiş Saç Kesimi",
          isEligible: true,
          unavailableReason: null,
        },
      ],
    });
    api.archiveCustomer.mockResolvedValue({
      ...existingCustomer,
      isActive: false,
    });
    api.reactivateCustomer.mockResolvedValue(existingCustomer);
  });

  async function openForm(): Promise<void> {
    render(<App />);
    fireEvent.click(
      screen.getAllByRole("button", { name: "+ Yeni Randevu" }).at(-1)!,
    );
    await screen.findByRole("heading", { name: "Yeni Randevu" });
  }

  async function fillRequiredFields(includeCustomer = true): Promise<void> {
    if (includeCustomer)
      fireEvent.change(screen.getByLabelText("Müşteri adı"), {
        target: { value: "Deniz Kaya" },
      });
    fireEvent.click(
      await screen.findByRole("checkbox", { name: /Saç Kesimi/ }),
    );
    fireEvent.change(screen.getByLabelText("Personel"), {
      target: { value: "staff-1" },
    });
    fireEvent.change(screen.getByLabelText("Saat"), {
      target: { value: "10:00" },
    });
  }

  it("opens required fields and lets the user cancel", async () => {
    await openForm();
    expect(screen.getByLabelText("Müşteri adı")).toBeInTheDocument();
    expect(screen.getByLabelText(/Telefon/)).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Hizmet" })).toBeInTheDocument();
    fireEvent.click(screen.getAllByRole("button", { name: "Vazgeç" })[0]);
    expect(
      screen.queryByRole("heading", { name: "Yeni Randevu" }),
    ).not.toBeInTheDocument();
  });

  it.each([
    ["Müşteriler", "Müşteriler"],
    ["Takvim", "Randevu takvimi"],
    ["Hizmetler", "Hizmetler"],
    ["Personel", "Personel"],
  ])("leaves the booking draft for the %s root screen", async (item, region) => {
    await openForm();
    fireEvent.click(screen.getByRole("button", { name: item }));
    expect(await screen.findByRole("region", { name: region })).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Yeni Randevu" }),
    ).not.toBeInTheDocument();
  });

  it("keeps a dirty booking open until the user chooses to leave", async () => {
    await openForm();
    fireEvent.change(screen.getByLabelText("Müşteri adı"), {
      target: { value: "Deniz Kaya" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Hizmetler" }));
    expect(
      await screen.findByRole("heading", { name: "Kaydedilmemiş bilgiler var." }),
    ).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 1, name: "Bugün" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Yeni Randevu" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Kal" }));
    expect(screen.getByLabelText("Müşteri adı")).toHaveValue("Deniz Kaya");
    expect(screen.getByRole("heading", { level: 1, name: "Bugün" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Hizmetler" }));
    fireEvent.click(await screen.findByRole("button", { name: "Çık" }));
    expect(await screen.findByRole("region", { name: "Hizmetler" })).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Yeni Randevu" }),
    ).not.toBeInTheDocument();
  });

  it("shows a bounded active customer list when the name field receives focus", async () => {
    api.searchCustomers.mockResolvedValue([
      existingCustomer,
      {
        ...existingCustomer,
        id: "customer-archived",
        firstName: "Arşiv",
        lastName: "Müşteri",
        isActive: false,
      },
    ]);
    await openForm();
    fireEvent.focus(screen.getByLabelText("Müşteri adı"));
    await waitFor(() => expect(api.searchCustomers).toHaveBeenCalledWith(""));
    expect(await screen.findByRole("list", { name: "Müşteri sonuçları" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Ayşe Yılmaz/ })).toBeInTheDocument();
    expect(screen.queryByText("Arşiv Müşteri")).not.toBeInTheDocument();
  });

  it("searches bounded active customers and uses an existing customer id", async () => {
    await openForm();
    fireEvent.change(screen.getByLabelText("Müşteri adı"), {
      target: { value: "Ayşe" },
    });
    await waitFor(() =>
      expect(api.searchCustomers).toHaveBeenCalledWith("Ayşe"),
    );
    fireEvent.click(await screen.findByRole("button", { name: /Ayşe Yılmaz/ }));
    await fillRequiredFields(false);
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    await waitFor(() =>
      expect(api.createAppointment).toHaveBeenCalledWith(
        expect.objectContaining({
          customerId: "customer-1",
          serviceIds: ["service-1"],
        }),
      ),
    );
    expect(api.createCustomer).not.toHaveBeenCalled();
  });

  it("searches existing customers by phone", async () => {
    await openForm();
    fireEvent.change(screen.getByLabelText("Müşteri adı"), {
      target: { value: "5551112233" },
    });
    await waitFor(() =>
      expect(api.searchCustomers).toHaveBeenCalledWith("5551112233"),
    );
  });

  it.each(["5551112233", "05551112233"])(
    "passes %s phone input to the bounded customer search",
    async (phone) => {
      await openForm();
      fireEvent.change(screen.getByLabelText("Müşteri adı"), {
        target: { value: phone },
      });
      await waitFor(() => expect(api.searchCustomers).toHaveBeenCalledWith(phone));
    },
  );

  it("keeps only the latest customer-search response", async () => {
    let resolveEmpty: ((customers: Array<typeof existingCustomer>) => void) | undefined;
    let resolveSpecific: ((customers: Array<typeof existingCustomer>) => void) | undefined;
    const unrelated = {
      ...existingCustomer,
      id: "customer-google-sync",
      firstName: "Test Google",
      lastName: "Sync",
    };
    const target = {
      ...existingCustomer,
      id: "customer-target",
      firstName: "Test",
      lastName: "Müşterisi2",
    };
    api.searchCustomers.mockImplementation((query: string) =>
      new Promise<Array<typeof existingCustomer>>((resolve) => {
        if (query === "") resolveEmpty = resolve;
        if (query === "Test Müşterisi2") resolveSpecific = resolve;
      }),
    );
    await openForm();
    fireEvent.focus(screen.getByLabelText("Müşteri adı"));
    await waitFor(() => expect(api.searchCustomers).toHaveBeenCalledWith(""));
    fireEvent.change(screen.getByLabelText("Müşteri adı"), {
      target: { value: "Test Müşterisi2" },
    });
    await waitFor(() =>
      expect(api.searchCustomers).toHaveBeenCalledWith("Test Müşterisi2"),
    );

    await act(async () => resolveSpecific?.([target]));
    expect(await screen.findByRole("button", { name: /Test Müşterisi2/ })).toBeInTheDocument();
    await act(async () => resolveEmpty?.([unrelated]));
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: /Test Google Sync/ })).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("button", { name: /Test Müşterisi2/ })).toBeInTheDocument();
  });

  it("keeps booking and customer-list search state independent", async () => {
    await openForm();
    fireEvent.change(screen.getByLabelText("Müşteri adı"), {
      target: { value: "Test Müşterisi2" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Müşteriler" }));
    fireEvent.click(await screen.findByRole("button", { name: "Çık" }));
    expect(await screen.findByLabelText("Müşteri ara")).toHaveValue("");

    fireEvent.change(screen.getByLabelText("Müşteri ara"), {
      target: { value: "Ayşe" },
    });
    fireEvent.click(
      screen.getAllByRole("button", { name: "+ Yeni Randevu" }).at(-1)!,
    );
    expect(await screen.findByLabelText("Müşteri adı")).toHaveValue("");
  });

  it("creates a phone-less new customer once, then creates an appointment", async () => {
    await openForm();
    await fillRequiredFields();
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    await waitFor(() =>
      expect(api.createCustomer).toHaveBeenCalledWith(
        expect.objectContaining({ phone: null }),
      ),
    );
    await waitFor(() =>
      expect(api.createAppointment).toHaveBeenCalledWith(
        expect.objectContaining({ customerId: "customer-new" }),
      ),
    );
    expect(screen.getByText("Randevu kaydedildi.")).toBeInTheDocument();
  });

  it("shows only the appointment WhatsApp preference in the booking form", async () => {
    await openForm();
    expect(
      screen.queryByLabelText("WhatsApp ile randevu hatırlatması gönderilebilir"),
    ).not.toBeInTheDocument();
    expect(
      screen.getByLabelText("Bu randevu için WhatsApp hatırlatması"),
    ).toBeChecked();
  });

  it("shows only active services and staff", async () => {
    await openForm();
    expect(
      await screen.findByRole("checkbox", { name: /Saç Kesimi/ }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("checkbox", { name: /Eski Hizmet/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("option", { name: "Ece Demir" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("option", { name: "Eski Personel" }),
    ).not.toBeInTheDocument();
  });

  it("passes multiple selected active services to the existing create contract", async () => {
    await openForm();
    await fillRequiredFields();
    fireEvent.click(screen.getByRole("checkbox", { name: /Fön/ }));
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    await waitFor(() =>
      expect(api.createAppointment).toHaveBeenCalledWith(
        expect.objectContaining({ serviceIds: ["service-1", "service-3"] }),
      ),
    );
  });

  it("defaults the appointment WhatsApp preference to enabled and persists an explicit opt-out", async () => {
    await openForm();
    expect(
      screen.getByLabelText("Bu randevu için WhatsApp hatırlatması"),
    ).toBeChecked();
    await fillRequiredFields();
    fireEvent.click(
      screen.getByLabelText("Bu randevu için WhatsApp hatırlatması"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    await waitFor(() =>
      expect(api.createAppointment).toHaveBeenCalledWith(
        expect.objectContaining({ whatsappReminderEnabled: false }),
      ),
    );
  });

  it("shows the Today WhatsApp indicator only when the effective preference is enabled", async () => {
    api.listAppointmentsByDate.mockResolvedValueOnce([
      todayAppointment,
      { ...todayAppointment, id: "appointment-disabled", whatsappReminderEffective: false },
    ]);
    render(<App />);
    await screen.findAllByText("Ayşe Yılmaz");
    expect(
      screen.getAllByLabelText("WhatsApp hatırlatması uygun"),
    ).toHaveLength(1);
  });

  it("disables saving while a create request is pending", async () => {
    let resolveCreate: ((value: unknown) => void) | undefined;
    api.createAppointment.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveCreate = resolve;
        }),
    );
    await openForm();
    await fillRequiredFields();
    const save = screen.getByRole("button", { name: "Randevuyu Kaydet" });
    fireEvent.click(save);
    await waitFor(() => expect(api.createAppointment).toHaveBeenCalledTimes(1));
    expect(
      screen.getByRole("button", { name: "Kaydediliyor..." }),
    ).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Kaydediliyor..." }));
    expect(api.createAppointment).toHaveBeenCalledTimes(1);
    await act(async () => {
      resolveCreate?.({ id: "appointment-pending" });
    });
  });

  it("keeps form data and maps domain errors after a failed create", async () => {
    api.createAppointment.mockRejectedValueOnce(
      new Error("APPOINTMENT_CONFLICT"),
    );
    await openForm();
    await fillRequiredFields();
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Bu personelin seçilen saatte başka bir randevusu var.",
    );
    expect(screen.getByLabelText("Müşteri adı")).toHaveValue("Deniz Kaya");
    expect(screen.getByLabelText("Saat")).toHaveValue("10:00");
  });

  it.each([
    [
      "OUTSIDE_WORKING_HOURS",
      "Seçilen saat personelin çalışma saatleri dışında.",
    ],
    ["STAFF_TIME_OFF", "Personel seçilen tarih veya saatte izinli."],
  ])("maps %s to a normal-user message", async (code, expected) => {
    api.createAppointment.mockRejectedValueOnce(new Error(code));
    await openForm();
    await fillRequiredFields();
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(expected);
  });

  it("does not duplicate a newly created customer on retry after appointment validation fails", async () => {
    api.createAppointment.mockRejectedValueOnce(
      new Error("APPOINTMENT_CONFLICT"),
    );
    await openForm();
    await fillRequiredFields();
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    await waitFor(() => expect(api.createCustomer).toHaveBeenCalledTimes(1));
  });

  it("maps duplicate phone errors", async () => {
    api.createCustomer.mockRejectedValueOnce(
      new Error("CONFLICT: CUSTOMER_PHONE_CONFLICT"),
    );
    await openForm();
    await fillRequiredFields();
    fireEvent.change(screen.getByLabelText(/Telefon/), {
      target: { value: "05551112233" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Bu telefon numarası başka bir müşteride kayıtlı.",
    );
  });

  it("shows the new-customer name error only for an incomplete new customer", async () => {
    await openForm();
    fireEvent.change(screen.getByLabelText("Müşteri adı"), {
      target: { value: "Deniz" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Müşteri adı ve soyadı girilmelidir.",
    );
    expect(api.createCustomer).not.toHaveBeenCalled();
  });

  it("places a failed save error next to the save controls and preserves the draft", async () => {
    api.createAppointment.mockRejectedValueOnce(new Error("APPOINTMENT_CONFLICT"));
    await openForm();
    await fillRequiredFields();
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    const error = await screen.findByRole("alert");
    const booking = error.closest(".booking-surface");
    expect(error.nextElementSibling).toBe(
      booking?.querySelector(".booking-actions"),
    );
    expect(screen.getByLabelText("Müşteri adı")).toHaveValue("Deniz Kaya");
  });
});

describe("Today appointment timeline", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.listAppointmentsByDate.mockResolvedValue([todayAppointment]);
    api.listAppointmentsByDateRange.mockResolvedValue([]);
    api.listActiveServices.mockResolvedValue([activeService]);
    api.listActiveStaff.mockResolvedValue([activeStaff]);
    api.createCustomer.mockResolvedValue({
      id: "customer-new",
      firstName: "Deniz",
      lastName: "Kaya",
      phone: null,
      isActive: true,
    });
    api.createAppointment.mockResolvedValue({ id: "appointment-new" });
    api.updateAppointment.mockResolvedValue(todayAppointment);
    api.searchCustomersByStatus.mockResolvedValue([existingCustomer]);
    api.getCustomerHistory.mockResolvedValue(customerHistory);
    api.getRepeatBookingSeed.mockResolvedValue({
      customerId: "customer-1",
      customerDisplayName: "Ayşe Yılmaz",
      staffId: "staff-1",
      services: [],
    });
    api.archiveCustomer.mockResolvedValue({
      ...existingCustomer,
      isActive: false,
    });
    api.reactivateCustomer.mockResolvedValue(existingCustomer);
  });

  it("renders historical snapshot services and Turkish status for each same-time appointment", async () => {
    api.listAppointmentsByDate.mockResolvedValue([
      todayAppointment,
      {
        ...todayAppointment,
        id: "appointment-2",
        customerName: "Deniz Kaya",
        staffId: "staff-2",
        staffName: "Mina Akın",
        status: "completed",
      },
    ]);
    render(<App />);
    expect(await screen.findByText("Ayşe Yılmaz")).toBeInTheDocument();
    expect(screen.getByText("Deniz Kaya")).toBeInTheDocument();
    expect(screen.getAllByText("Geçmiş Saç Kesimi")).toHaveLength(2);
    expect(screen.getByText("Ece Demir · 45 dk")).toBeInTheDocument();
    expect(screen.getByText("Mina Akın · 45 dk")).toBeInTheDocument();
    expect(screen.getByText("Planlandı")).toBeInTheDocument();
    expect(
      screen.getByText("Tamamlandı", { selector: "span" }),
    ).toBeInTheDocument();
  });

  it.each([
    ["Tamamlandı", "completed"],
    ["Gelmedi", "no_show"],
    ["İptal", "cancelled"],
  ])(
    "updates a planned appointment to %s through the full typed contract",
    async (label, status) => {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: label }));
      await waitFor(() =>
        expect(api.updateAppointment).toHaveBeenCalledWith(
          "appointment-1",
          expect.objectContaining({ status, serviceIds: ["service-1"] }),
        ),
      );
    },
  );

  it("keeps the timeline visible and reports a status failure", async () => {
    api.updateAppointment.mockRejectedValueOnce(new Error("STAFF_TIME_OFF"));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "İptal" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Personel seçilen tarih veya saatte izinli.",
    );
    expect(screen.getByText("Ayşe Yılmaz")).toBeInTheDocument();
  });

  it("keeps status corrections available after a completed or cancelled status", async () => {
    api.listAppointmentsByDate.mockResolvedValueOnce([
      { ...todayAppointment, status: "completed" },
    ]);
    render(<App />);
    await screen.findByText("Ayşe Yılmaz");
    expect(screen.getByRole("button", { name: "Gelmedi" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Gelmedi" }));
    await waitFor(() =>
      expect(api.updateAppointment).toHaveBeenCalledWith(
        "appointment-1",
        expect.objectContaining({ status: "no_show" }),
      ),
    );
  });

  it("shows the availability reason when cancelled is corrected to active", async () => {
    api.listAppointmentsByDate.mockResolvedValueOnce([
      { ...todayAppointment, status: "cancelled" },
    ]);
    api.updateAppointment.mockRejectedValueOnce(new Error("APPOINTMENT_CONFLICT"));
    render(<App />);
    await screen.findByText("Ayşe Yılmaz");
    fireEvent.click(screen.getByRole("button", { name: "Tamamlandı" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Bu personelin seçilen saatte başka bir randevusu var.",
    );
  });

  it("refreshes the bounded today list after a successful new appointment", async () => {
    api.listAppointmentsByDate
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([todayAppointment]);
    render(<App />);
    await screen.findByText("Bugün için randevu yok");
    fireEvent.click(screen.getByRole("button", { name: "+ Yeni Randevu" }));
    await screen.findByRole("heading", { name: "Yeni Randevu" });
    fireEvent.change(screen.getByLabelText("Müşteri adı"), {
      target: { value: "Deniz Kaya" },
    });
    fireEvent.click(
      await screen.findByRole("checkbox", { name: /Saç Kesimi/ }),
    );
    fireEvent.change(screen.getByLabelText("Personel"), {
      target: { value: "staff-1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    expect(await screen.findByText("Ayşe Yılmaz")).toBeInTheDocument();
    expect(api.listAppointmentsByDate).toHaveBeenCalledTimes(2);
  });
});

describe("Calendar day, week and edit foundation", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.listAppointmentsByDate.mockResolvedValue([]);
    api.listAppointmentsByDateRange.mockResolvedValue([todayAppointment]);
    api.listActiveServices.mockResolvedValue([
      activeService,
      secondActiveService,
      inactiveService,
    ]);
    api.listActiveStaff.mockResolvedValue([activeStaff, inactiveStaff]);
    api.updateAppointment.mockResolvedValue(todayAppointment);
  });

  async function openCalendar(): Promise<void> {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Takvim" }));
    await screen.findByRole("region", { name: "Randevu takvimi" });
  }

  it("opens calendar in day mode and uses one bounded range read", async () => {
    await openCalendar();
    expect(screen.getByRole("button", { name: "Gün" })).toHaveClass(
      "is-active",
    );
    await waitFor(() =>
      expect(api.listAppointmentsByDateRange).toHaveBeenCalledTimes(1),
    );
    expect(api.listAppointmentsByDateRange).toHaveBeenCalledWith(
      expect.any(String),
      expect.any(String),
    );
    expect(screen.getByText("Ayşe Yılmaz")).toBeInTheDocument();
  });

  it("switches to a Monday-through-Saturday week view without a horizontal scroll surface", async () => {
    await openCalendar();
    fireEvent.click(screen.getByRole("button", { name: "Hafta" }));
    await waitFor(() =>
      expect(api.listAppointmentsByDateRange).toHaveBeenCalledTimes(2),
    );
    expect(screen.getAllByText("Randevu yok").length).toBeGreaterThan(0);
    const weekGrid = screen.getByRole("region", { name: "Randevu takvimi" })
      .querySelector(".calendar-week-grid")!;
    expect(weekGrid.querySelectorAll(".calendar-week-day")).toHaveLength(6);
    expect(
      Array.from(weekGrid.querySelectorAll("h3")).some((heading) =>
        heading.textContent?.endsWith(" Pazar"),
      ),
    ).toBe(false);
    expect(weekGrid).not.toHaveStyle({ overflowX: "auto" });
  });

  it("navigates previous, today and next using bounded range reloads", async () => {
    await openCalendar();
    fireEvent.click(screen.getByRole("button", { name: "Sonraki" }));
    await waitFor(() =>
      expect(api.listAppointmentsByDateRange).toHaveBeenCalledTimes(2),
    );
    fireEvent.click(screen.getByRole("button", { name: "Önceki" }));
    await waitFor(() =>
      expect(api.listAppointmentsByDateRange).toHaveBeenCalledTimes(3),
    );
    fireEvent.click(
      screen
        .getByRole("region", { name: "Randevu takvimi" })
        .querySelector(".calendar-navigation button:nth-child(2)")!,
    );
    await waitFor(() =>
      expect(api.listAppointmentsByDateRange).toHaveBeenCalledTimes(4),
    );
  });

  it("opens editing only from the explicit action, preserves choices, and saves the full update", async () => {
    await openCalendar();
    fireEvent.click(await screen.findByText("Ayşe Yılmaz"));
    expect(
      screen.queryByRole("heading", { name: "Randevuyu Düzenle" }),
    ).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Düzenle" }));
    expect(
      await screen.findByRole("heading", { name: "Randevuyu Düzenle" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("option", { name: "Eski Personel" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("checkbox", { name: "Düzenle Eski Hizmet" }),
    ).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Düzenle tarih"), {
      target: { value: "2026-09-02" },
    });
    fireEvent.change(screen.getByLabelText("Düzenle saat"), {
      target: { value: "11:30" },
    });
    fireEvent.click(screen.getByRole("checkbox", { name: "Düzenle Fön" }));
    fireEvent.click(
      screen.getByLabelText("Bu randevu için WhatsApp hatırlatması"),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Değişiklikleri Kaydet" }),
    );
    await waitFor(() =>
      expect(api.updateAppointment).toHaveBeenCalledWith(
        "appointment-1",
        expect.objectContaining({
          localDate: "2026-09-02",
          localStartTime: "11:30",
          serviceIds: ["service-1", "service-3"],
          whatsappReminderEnabled: false,
        }),
      ),
    );
  });

  it("keeps the Week edit action in its own stable card row", async () => {
    await openCalendar();
    fireEvent.click(screen.getByRole("button", { name: "Hafta" }));
    fireEvent.click(screen.getByRole("button", { name: "Önceki" }));
    const edit = await screen.findByRole("button", { name: "Düzenle" });
    const card = edit.closest("article");
    expect(card).toHaveClass("calendar-appointment");
    expect(card?.querySelector(".calendar-appointment-details")).not.toBeNull();
    expect(edit).toHaveClass("calendar-edit-action");
  });

  it("keeps a dirty appointment edit open until the user chooses to leave", async () => {
    await openCalendar();
    fireEvent.click(await screen.findByRole("button", { name: "Düzenle" }));
    await screen.findByRole("heading", { name: "Randevuyu Düzenle" });
    fireEvent.change(screen.getByLabelText("Düzenle saat"), {
      target: { value: "13:00" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Müşteriler" }));
    expect(
      await screen.findByRole("heading", { name: "Kaydedilmemiş bilgiler var." }),
    ).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 1, name: "Takvim" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Kal" }));
    expect(screen.getByLabelText("Düzenle saat")).toHaveValue("13:00");
    fireEvent.click(screen.getByRole("button", { name: "Müşteriler" }));
    fireEvent.click(await screen.findByRole("button", { name: "Çık" }));
    expect(await screen.findByRole("region", { name: "Müşteriler" })).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Randevuyu Düzenle" }),
    ).not.toBeInTheDocument();
  });

  it("keeps the edit draft open and maps availability failures", async () => {
    api.updateAppointment.mockRejectedValueOnce(new Error("STAFF_TIME_OFF"));
    await openCalendar();
    fireEvent.click(await screen.findByRole("button", { name: "Düzenle" }));
    await screen.findByRole("heading", { name: "Randevuyu Düzenle" });
    fireEvent.change(screen.getByLabelText("Düzenle saat"), {
      target: { value: "13:00" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Değişiklikleri Kaydet" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Personel seçilen tarih veya saatte izinli.",
    );
    expect(screen.getByLabelText("Düzenle saat")).toHaveValue("13:00");
  });

  it("maps a Tauri string conflict rejection and preserves the edit draft", async () => {
    api.updateAppointment.mockRejectedValueOnce(
      "CONFLICT: APPOINTMENT_CONFLICT",
    );
    await openCalendar();
    fireEvent.click(await screen.findByRole("button", { name: "Düzenle" }));
    await screen.findByRole("heading", { name: "Randevuyu Düzenle" });
    fireEvent.change(screen.getByLabelText("Düzenle saat"), {
      target: { value: "13:00" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Değişiklikleri Kaydet" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Bu personelin seçilen saatte başka bir randevusu var.",
    );
    expect(screen.getByLabelText("Düzenle saat")).toHaveValue("13:00");
  });

  it("maps an Error conflict rejection", async () => {
    api.updateAppointment.mockRejectedValueOnce(
      new Error("CONFLICT: APPOINTMENT_CONFLICT"),
    );
    await openCalendar();
    fireEvent.click(await screen.findByRole("button", { name: "Düzenle" }));
    await screen.findByRole("heading", { name: "Randevuyu Düzenle" });
    fireEvent.click(
      screen.getByRole("button", { name: "Değişiklikleri Kaydet" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Bu personelin seçilen saatte başka bir randevusu var.",
    );
  });

  it("keeps an unknown Tauri rejection behind the generic message", async () => {
    api.updateAppointment.mockRejectedValueOnce("UNEXPECTED_BACKEND_FAILURE");
    await openCalendar();
    fireEvent.click(await screen.findByRole("button", { name: "Düzenle" }));
    await screen.findByRole("heading", { name: "Randevuyu Düzenle" });
    fireEvent.click(
      screen.getByRole("button", { name: "Değişiklikleri Kaydet" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Randevu kaydedilemedi. Lütfen bilgileri kontrol edip tekrar deneyin.",
    );
  });

  it("cancels an edit without a mutation", async () => {
    await openCalendar();
    fireEvent.click(await screen.findByRole("button", { name: "Düzenle" }));
    await screen.findByRole("heading", { name: "Randevuyu Düzenle" });
    fireEvent.click(screen.getAllByRole("button", { name: "Vazgeç" }).at(-1)!);
    expect(
      screen.queryByRole("heading", { name: "Randevuyu Düzenle" }),
    ).not.toBeInTheDocument();
    expect(api.updateAppointment).not.toHaveBeenCalled();
  });
});

describe("Customer list, history and repeat booking", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.listAppointmentsByDate.mockResolvedValue([]);
    api.listAppointmentsByDateRange.mockResolvedValue([]);
    api.listActiveServices.mockResolvedValue([
      activeService,
      secondActiveService,
      inactiveService,
    ]);
    api.listActiveStaff.mockResolvedValue([activeStaff, inactiveStaff]);
    api.searchCustomersByStatus.mockResolvedValue([
      existingCustomer,
      {
        id: "customer-2",
        firstName: "Telefon",
        lastName: "Yok",
        phone: null,
        isActive: true,
      },
    ]);
    api.getCustomerHistory.mockResolvedValue(customerHistory);
    api.getRepeatBookingSeed.mockResolvedValue({
      customerId: "customer-1",
      customerDisplayName: "Ayşe Yılmaz",
      staffId: "staff-1",
      services: [
        {
          serviceId: "service-1",
          serviceNameSnapshot: "Geçmiş Saç Kesimi",
          isEligible: true,
          unavailableReason: null,
        },
        {
          serviceId: "service-2",
          serviceNameSnapshot: "Eski Hizmet",
          isEligible: false,
          unavailableReason: "SERVICE_INACTIVE",
        },
      ],
    });
    api.archiveCustomer.mockResolvedValue({
      ...existingCustomer,
      isActive: false,
    });
    api.reactivateCustomer.mockResolvedValue(existingCustomer);
    api.updateCustomer.mockResolvedValue(existingCustomer);
    api.createAppointment.mockResolvedValue({ id: "appointment-new" });
  });

  async function openCustomers(): Promise<void> {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Müşteriler" }));
    await screen.findByRole("region", { name: "Müşteriler" });
  }

  it("shows bounded active customers and can include archived records", async () => {
    await openCustomers();
    expect(await screen.findByText("Ayşe Yılmaz")).toBeInTheDocument();
    expect(screen.queryByText("Telefon yok")).not.toBeInTheDocument();
    await waitFor(() =>
      expect(api.searchCustomersByStatus).toHaveBeenCalledWith("", "active"),
    );
    fireEvent.click(
      screen.getByRole("checkbox", { name: "Arşivdekileri göster" }),
    );
    await waitFor(() =>
      expect(api.searchCustomersByStatus).toHaveBeenLastCalledWith("", "all"),
    );
  });

  it("uses server-side name and phone searches", async () => {
    await openCustomers();
    fireEvent.change(screen.getByLabelText("Müşteri ara"), {
      target: { value: "5551112233" },
    });
    await waitFor(() =>
      expect(api.searchCustomersByStatus).toHaveBeenLastCalledWith(
        "5551112233",
        "active",
      ),
    );
  });

  it("creates an independent customer with an optional phone and refreshes the list", async () => {
    const created = {
      ...existingCustomer,
      id: "customer-new",
      firstName: "Deniz",
      lastName: "Kaya",
      phone: null,
      notes: "Yeni not",
    };
    api.searchCustomersByStatus
      .mockResolvedValueOnce([existingCustomer])
      .mockResolvedValueOnce([existingCustomer, created]);
    api.createCustomer.mockResolvedValue(created);
    await openCustomers();
    fireEvent.click(screen.getByRole("button", { name: "Yeni Müşteri" }));
    fireEvent.change(screen.getByLabelText("Müşteri adı"), {
      target: { value: "Deniz" },
    });
    fireEvent.change(screen.getByLabelText("Müşteri soyadı"), {
      target: { value: "Kaya" },
    });
    fireEvent.change(screen.getByLabelText("Müşteri notu"), {
      target: { value: "Yeni not" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Müşteri Ekle" }));
    await waitFor(() =>
      expect(api.createCustomer).toHaveBeenCalledWith(
        expect.objectContaining({
          firstName: "Deniz",
          lastName: "Kaya",
          phone: null,
          notes: "Yeni not",
        }),
      ),
    );
    await screen.findByText("Deniz Kaya");
    expect(api.createAppointment).not.toHaveBeenCalled();
  });

  it("keeps an independent customer draft and shows the safe duplicate phone error", async () => {
    api.createCustomer.mockRejectedValueOnce(
      new Error("CONFLICT: CUSTOMER_PHONE_CONFLICT"),
    );
    await openCustomers();
    fireEvent.click(screen.getByRole("button", { name: "Yeni Müşteri" }));
    fireEvent.change(screen.getByLabelText("Müşteri adı"), {
      target: { value: "Deniz" },
    });
    fireEvent.change(screen.getByLabelText("Müşteri soyadı"), {
      target: { value: "Kaya" },
    });
    fireEvent.change(screen.getByLabelText("Müşteri telefonu"), {
      target: { value: "05551112233" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Müşteri Ekle" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Bu telefon numarası başka bir müşteride kayıtlı.",
    );
    expect(screen.getByLabelText("Müşteri telefonu")).toHaveValue("05551112233");
  });

  it("loads, updates, and clears customer phone and notes", async () => {
    const updated = { ...existingCustomer, phone: null, notes: null };
    api.updateCustomer.mockResolvedValue(updated);
    await openCustomers();
    fireEvent.click(await screen.findByRole("button", { name: /Ayşe Yılmaz/ }));
    await screen.findByRole("heading", { name: "Randevu Geçmişi" });
    fireEvent.click(screen.getByRole("button", { name: "Düzenle" }));
    expect(screen.getByLabelText("Müşteri adı")).toHaveValue("Ayşe");
    expect(screen.getByLabelText("Müşteri telefonu")).toHaveValue("+905551112233");
    expect(screen.getByLabelText("Müşteri notu")).toHaveValue("Eski not");
    fireEvent.change(screen.getByLabelText("Müşteri telefonu"), {
      target: { value: "" },
    });
    fireEvent.change(screen.getByLabelText("Müşteri notu"), {
      target: { value: "" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Müşteriyi Güncelle" }));
    await waitFor(() =>
      expect(api.updateCustomer).toHaveBeenCalledWith(
        "customer-1",
        expect.objectContaining({ phone: null, notes: null }),
      ),
    );
  });

  it("preserves an edit draft after a customer update failure", async () => {
    api.updateCustomer.mockRejectedValueOnce(
      new Error("CONFLICT: CUSTOMER_PHONE_CONFLICT"),
    );
    await openCustomers();
    fireEvent.click(await screen.findByRole("button", { name: /Ayşe Yılmaz/ }));
    await screen.findByRole("heading", { name: "Randevu Geçmişi" });
    fireEvent.click(screen.getByRole("button", { name: "Düzenle" }));
    fireEvent.change(screen.getByLabelText("Müşteri notu"), {
      target: { value: "Değişen not" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Müşteriyi Güncelle" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Bu telefon numarası başka bir müşteride kayıtlı.",
    );
    expect(screen.getByLabelText("Müşteri notu")).toHaveValue("Değişen not");
  });

  it("opens snapshot history and renders all historical services", async () => {
    await openCustomers();
    fireEvent.click(await screen.findByRole("button", { name: /Ayşe Yılmaz/ }));
    expect(
      await screen.findByRole("heading", { name: "Randevu Geçmişi" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Geçmiş Saç Kesimi, Geçmiş Fön"),
    ).toBeInTheDocument();
    expect(screen.getByText("Tamamlandı")).toBeInTheDocument();
    expect(api.getCustomerHistory).toHaveBeenCalledWith("customer-1", 10, 0);
  });

  it("leaves customer detail for the selected root screen", async () => {
    await openCustomers();
    fireEvent.click(await screen.findByRole("button", { name: /Ayşe Yılmaz/ }));
    await screen.findByRole("heading", { name: "Randevu Geçmişi" });
    fireEvent.click(screen.getByRole("button", { name: "Takvim" }));
    expect(
      await screen.findByRole("region", { name: "Randevu takvimi" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Randevu Geçmişi" }),
    ).not.toBeInTheDocument();
  });

  it("loads the next bounded history page only when requested", async () => {
    const firstPage = {
      ...customerHistory,
      appointments: Array.from({ length: 10 }, (_, index) => ({
        ...customerHistory.appointments[0],
        appointmentId: `history-${index}`,
      })),
    };
    const nextPage = {
      ...customerHistory,
      appointments: [
        { ...customerHistory.appointments[0], appointmentId: "history-next" },
      ],
      offset: 10,
    };
    api.getCustomerHistory
      .mockResolvedValueOnce(firstPage)
      .mockResolvedValueOnce(nextPage);
    await openCustomers();
    fireEvent.click(await screen.findByRole("button", { name: /Ayşe Yılmaz/ }));
    await screen.findByRole("button", { name: "Daha Fazla Göster" });
    fireEvent.click(screen.getByRole("button", { name: "Daha Fazla Göster" }));
    await waitFor(() =>
      expect(api.getCustomerHistory).toHaveBeenLastCalledWith(
        "customer-1",
        10,
        10,
      ),
    );
    expect(screen.getAllByText("Geçmiş Saç Kesimi, Geçmiş Fön")).toHaveLength(
      11,
    );
  });

  it("reuses the booking form for a selected customer without creating another customer", async () => {
    await openCustomers();
    fireEvent.click(await screen.findByRole("button", { name: /Ayşe Yılmaz/ }));
    await screen.findByRole("heading", { name: "Randevu Geçmişi" });
    fireEvent.click(
      screen.getAllByRole("button", { name: "+ Yeni Randevu" }).at(-1)!,
    );
    expect(
      await screen.findByRole("heading", { name: "Yeni Randevu" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Müşteri adı")).toHaveValue("Ayşe Yılmaz");
    fireEvent.click(
      await screen.findByRole("checkbox", { name: /Saç Kesimi/ }),
    );
    fireEvent.change(screen.getByLabelText("Personel"), {
      target: { value: "staff-1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Randevuyu Kaydet" }));
    await waitFor(() =>
      expect(api.createAppointment).toHaveBeenCalledWith(
        expect.objectContaining({ customerId: "customer-1" }),
      ),
    );
    expect(api.createCustomer).not.toHaveBeenCalled();
  });

  it("uses repeat seed without copying date, time, or inactive services", async () => {
    await openCustomers();
    fireEvent.click(await screen.findByRole("button", { name: /Ayşe Yılmaz/ }));
    await screen.findByRole("heading", { name: "Randevu Geçmişi" });
    fireEvent.click(screen.getByRole("button", { name: "Tekrar Randevu Ver" }));
    expect(
      await screen.findByRole("heading", { name: "Yeni Randevu" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Tarih")).toHaveValue("");
    expect(screen.getByLabelText("Saat")).toHaveValue("");
    expect(screen.getByRole("checkbox", { name: /Saç Kesimi/ })).toBeChecked();
    expect(
      screen.queryByRole("checkbox", { name: /Eski Hizmet/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText("Bu hizmet artık aktif değil. Yeni bir hizmet seçin."),
    ).toBeInTheDocument();
  });

  it("archives and reactivates through typed state transitions without delete", async () => {
    await openCustomers();
    fireEvent.click(await screen.findByRole("button", { name: /Ayşe Yılmaz/ }));
    await screen.findByRole("heading", { name: "Randevu Geçmişi" });
    fireEvent.click(screen.getByRole("button", { name: "Arşivle" }));
    await waitFor(() =>
      expect(api.archiveCustomer).toHaveBeenCalledWith("customer-1"),
    );
  });

  it("opens archived history and requires explicit reactivation before booking", async () => {
    const archivedHistory = {
      ...customerHistory,
      customer: { ...customerHistory.customer, isActive: false },
    };
    api.searchCustomersByStatus.mockResolvedValue([
      { ...existingCustomer, isActive: false },
    ]);
    api.getCustomerHistory.mockResolvedValue(archivedHistory);
    await openCustomers();
    fireEvent.click(
      screen.getByRole("checkbox", { name: "Arşivdekileri göster" }),
    );
    fireEvent.click(await screen.findByRole("button", { name: /Ayşe Yılmaz/ }));
    expect(
      await screen.findByText(
        "Yeni randevu için önce müşteriyi tekrar aktif edin.",
      ),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Tekrar Aktif Et" }));
    await waitFor(() =>
      expect(api.reactivateCustomer).toHaveBeenCalledWith("customer-1"),
    );
  });
});

describe("Services and staff management", () => {
  const managedService = {
    ...activeService,
    categoryId: "category-1",
    categoryName: "Saç",
    sortOrder: 10,
    availabilityStatus: "active",
  };
  const managedStaff = {
    ...activeStaff,
    phone: null,
    specialtyNote: "Kesim",
    colorKey: "sage",
    sortOrder: 10,
  };

  beforeEach(() => {
    vi.clearAllMocks();
    api.listAppointmentsByDate.mockResolvedValue([]);
    api.getBusinessProfile.mockResolvedValue({ currencyCode: "TRY" });
    api.listServices.mockResolvedValue([managedService]);
    api.listServiceCategories.mockResolvedValue([
      { id: "category-1", name: "Saç", sortOrder: 10, isActive: true },
    ]);
    api.createServiceCategory.mockResolvedValue({
      id: "category-2",
      name: "Cilt",
      sortOrder: 20,
      isActive: true,
    });
    api.updateServiceCategory.mockResolvedValue({
      id: "category-1",
      name: "Saç Bakımı",
      sortOrder: 10,
      isActive: true,
    });
    api.createService.mockResolvedValue(managedService);
    api.updateService.mockResolvedValue(managedService);
    api.deactivateService.mockResolvedValue({
      ...managedService,
      isActive: false,
    });
    api.reactivateService.mockResolvedValue(managedService);
    api.listStaff.mockResolvedValue([managedStaff]);
    api.createStaff.mockResolvedValue(managedStaff);
    api.updateStaff.mockResolvedValue(managedStaff);
    api.deactivateStaff.mockResolvedValue({ ...managedStaff, isActive: false });
    api.reactivateStaff.mockResolvedValue(managedStaff);
    api.listStaffServiceIds.mockResolvedValue(["service-1"]);
    api.setStaffServices.mockResolvedValue(["service-1"]);
    api.listActiveServices.mockResolvedValue([managedService]);
    api.listStaffWorkingHours.mockResolvedValue([]);
    api.setStaffWorkingHours.mockResolvedValue(undefined);
    api.isStaffScheduleUnrestricted.mockResolvedValue(true);
    api.listStaffTimeOff.mockResolvedValue([]);
    api.addStaffTimeOff.mockResolvedValue({
      id: "off-1",
      staffId: "staff-1",
      localDate: "2026-09-10",
      fullDay: true,
      startMinute: null,
      endMinute: null,
    });
    api.updateStaffTimeOff.mockResolvedValue(undefined);
    api.removeStaffTimeOff.mockResolvedValue(undefined);
  });

  it("lists, creates, edits and deactivates services with the business currency", async () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Hizmetler" }));
    await screen.findByRole("region", { name: "Hizmetler" });
    expect(await screen.findByText("Saç Kesimi")).toBeInTheDocument();
    expect(screen.getByText(/₺/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Hizmet adı"), {
      target: { value: "Boya" },
    });
    fireEvent.change(screen.getByLabelText("Hizmet süresi"), {
      target: { value: "60" },
    });
    fireEvent.change(screen.getByLabelText("Hizmet fiyatı"), {
      target: { value: "250,50" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Hizmet Ekle" }));
    await waitFor(() =>
      expect(api.createService).toHaveBeenCalledWith(
        expect.objectContaining({
          defaultPriceMinor: 25050,
          durationMinutes: 60,
        }),
      ),
    );
    fireEvent.click(
      within(screen.getByRole("region", { name: "Saç kategorisi" }))
        .getAllByRole("button", { name: "Düzenle" })
        .at(-1)!,
    );
    expect(screen.getByLabelText("Hizmet adı")).toHaveValue("Saç Kesimi");
    fireEvent.click(screen.getByRole("button", { name: "Pasife Al" }));
    await waitFor(() =>
      expect(api.deactivateService).toHaveBeenCalledWith("service-1"),
    );
  });

  it("creates a category and selects it for the service form", async () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Hizmetler" }));
    await screen.findByRole("region", { name: "Hizmetler" });
    fireEvent.change(screen.getByLabelText("Yeni kategori adı"), {
      target: { value: "Cilt" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Kategori Ekle" }));
    await waitFor(() =>
      expect(api.createServiceCategory).toHaveBeenCalledWith({
        name: "Cilt",
        isActive: true,
      }),
    );
    expect(
      within(screen.getByLabelText("Hizmet kategorisi")).getByRole("option", {
        name: "Cilt",
      }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Hizmet kategorisi")).toHaveValue("category-2");
  });

  it("groups services by category and filters a distinct Genel category", async () => {
    const generalService = {
      ...managedService,
      id: "service-2",
      name: "Manikür",
      categoryId: "category-2",
      categoryName: "Genel",
    };
    api.listServiceCategories.mockResolvedValue([
      { id: "category-1", name: "Saç", sortOrder: 10, isActive: true },
      { id: "category-2", name: "Genel", sortOrder: 20, isActive: true },
    ]);
    api.listServices.mockResolvedValue([managedService, generalService]);

    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Hizmetler" }));
    expect(await screen.findByRole("region", { name: "Saç kategorisi" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Genel kategorisi" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Tümü" })).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Hizmet kategorisi filtresi"), {
      target: { value: "category-2" },
    });
    expect(screen.queryByRole("region", { name: "Saç kategorisi" })).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Genel kategorisi" })).toHaveTextContent(
      "Manikür",
    );
  });

  it("renames a category without moving its services", async () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Hizmetler" }));
    const category = await screen.findByRole("region", { name: "Saç kategorisi" });
    fireEvent.click(
      within(category).getAllByRole("button", { name: "Düzenle" })[0],
    );
    fireEvent.change(screen.getByLabelText("Kategori adı"), {
      target: { value: "Saç Bakımı" },
    });
    fireEvent.click(within(category).getByRole("button", { name: "Kaydet" }));
    await waitFor(() =>
      expect(api.updateServiceCategory).toHaveBeenCalledWith("category-1", {
        name: "Saç Bakımı",
        isActive: true,
      }),
    );
    expect(await screen.findByRole("region", { name: "Saç Bakımı kategorisi" })).toHaveTextContent(
      "Saç Kesimi",
    );
  });

  it("moves a service by saving its one selected category", async () => {
    api.listServiceCategories.mockResolvedValue([
      { id: "category-1", name: "Saç", sortOrder: 10, isActive: true },
      { id: "category-2", name: "Genel", sortOrder: 20, isActive: true },
    ]);
    api.updateService.mockResolvedValue({
      ...managedService,
      categoryId: "category-2",
      categoryName: "Genel",
    });
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Hizmetler" }));
    const category = await screen.findByRole("region", { name: "Saç kategorisi" });
    fireEvent.click(
      within(category).getAllByRole("button", { name: "Düzenle" }).at(-1)!,
    );
    fireEvent.change(screen.getByLabelText("Hizmet kategorisi"), {
      target: { value: "category-2" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Hizmeti Güncelle" }));
    await waitFor(() =>
      expect(api.updateService).toHaveBeenCalledWith(
        "service-1",
        expect.objectContaining({ categoryId: "category-2" }),
      ),
    );
  });

  it("keeps inactive services out of new assignments and can persist staff assignment and hours", async () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Personel" }));
    await screen.findByRole("region", { name: "Personel" });
    fireEvent.click(await screen.findByRole("button", { name: /Ece Demir/ }));
    expect(
      await screen.findByText(/Henüz kısıt tanımlanmadı/),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("checkbox", { name: /Eski Hizmet/ }),
    ).not.toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Hizmet Atamalarını Kaydet" }),
    );
    await waitFor(() =>
      expect(api.setStaffServices).toHaveBeenCalledWith("staff-1", [
        "service-1",
      ]),
    );
    fireEvent.click(screen.getByRole("button", { name: "Aralık Ekle" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Çalışma Saatlerini Kaydet" }),
    );
    await waitFor(() =>
      expect(api.setStaffWorkingHours).toHaveBeenCalledWith(
        "staff-1",
        expect.arrayContaining([
          expect.objectContaining({
            weekday: 0,
            startMinute: 540,
            endMinute: 1020,
          }),
        ]),
      ),
    );
  });

  it("adds full-day and partial time off through typed commands", async () => {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Personel" }));
    fireEvent.click(await screen.findByRole("button", { name: /Ece Demir/ }));
    await screen.findByText("İzin ve kapalı günler");
    fireEvent.change(screen.getByLabelText("İzin tarihi"), {
      target: { value: "2026-09-10" },
    });
    fireEvent.click(screen.getByRole("button", { name: "İzin Ekle" }));
    await waitFor(() =>
      expect(api.addStaffTimeOff).toHaveBeenCalledWith("staff-1", {
        localDate: "2026-09-10",
        fullDay: true,
        startMinute: null,
        endMinute: null,
      }),
    );
  });
});

describe("Business and data settings", () => {
  const profile = {
    businessName: "Guzel Salon",
    phone: null,
    email: null,
    address: null,
    currencyCode: "TRY",
    themeKey: "default",
    logoMimeType: null,
    updatedAt: "2026-09-02T10:00:00Z",
  };

  beforeEach(() => {
    vi.clearAllMocks();
    api.listAppointmentsByDate.mockResolvedValue([]);
    api.getBusinessProfile.mockResolvedValue(profile);
    api.updateBusinessProfile.mockResolvedValue(profile);
    api.createManualBackup.mockResolvedValue({
      backupPath: "C:/safe/backup.sqlite",
    });
    api.restoreDatabaseBackup.mockResolvedValue(undefined);
    api.openDataFolder.mockResolvedValue(undefined);
    api.selectBackupFile.mockResolvedValue("C:/selected/backup.sqlite");
    api.getGoogleCalendarStatus.mockResolvedValue({
      configured: true,
      connected: false,
      pendingSyncCount: 0,
      blockedSyncCount: 0,
    });
    api.connectGoogleCalendar.mockResolvedValue({
      connected: true,
      calendarId: "primary",
    });
    api.disconnectGoogleCalendar.mockResolvedValue(true);
    api.getCloudConnectionStatus.mockResolvedValue({
      configured: true,
      sessionPresent: false,
    });
    api.getReminderReadiness.mockResolvedValue({
      state: "disconnected",
      automaticEnabled: false,
    });
    api.requestCloudOtp.mockResolvedValue(true);
    api.verifyCloudOtp.mockResolvedValue({
      configured: true,
      sessionPresent: true,
    });
  });

  async function openSettings(): Promise<void> {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Ayarlar" }));
    await screen.findByRole("region", { name: "Ayarlar" });
    await screen.findByLabelText("İşletme adı");
  }

  it("loads, saves, and reloads the typed business profile without secret fields", async () => {
    await openSettings();
    expect(screen.getByLabelText("İşletme adı")).toHaveValue("Guzel Salon");
    expect(screen.getByLabelText("Para birimi")).toHaveDisplayValue(
      "Türk Lirası (TRY)",
    );
    expect(
      screen.queryByText(
        /Supabase|OAuth Client ID|OAuth Secret|dispatcher|SQL/i,
      ),
    ).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("İşletme adı"), {
      target: { value: " Yeni Salon " },
    });
    fireEvent.change(screen.getByLabelText("İşletme telefonu"), {
      target: { value: "05551112233" },
    });
    fireEvent.change(screen.getByLabelText("İşletme e-postası"), {
      target: { value: "info@example.com" },
    });
    fireEvent.change(screen.getByLabelText("İşletme adresi"), {
      target: { value: "Merkez" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "İşletme Bilgilerini Kaydet" }),
    );
    await waitFor(() =>
      expect(api.updateBusinessProfile).toHaveBeenCalledWith(
        expect.objectContaining({
          businessName: " Yeni Salon ",
          phone: "05551112233",
          email: "info@example.com",
          address: "Merkez",
          currencyCode: "TRY",
          themeKey: "default",
        }),
      ),
    );
    expect(api.getBusinessProfile).toHaveBeenCalledTimes(2);
  });

  it("shows a safe profile failure and preserves the draft", async () => {
    await openSettings();
    api.updateBusinessProfile.mockRejectedValueOnce(new Error("validation"));
    fireEvent.change(screen.getByLabelText("İşletme adı"), {
      target: { value: "Taslak Salon" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "İşletme Bilgilerini Kaydet" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "İşletme bilgileri kaydedilemedi",
    );
    expect(screen.getByLabelText("İşletme adı")).toHaveValue("Taslak Salon");
  });

  it("creates one manual backup and prevents duplicate clicks while pending", async () => {
    let resolveBackup: ((value: unknown) => void) | undefined;
    api.createManualBackup.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveBackup = resolve;
        }),
    );
    await openSettings();
    fireEvent.click(screen.getByRole("button", { name: "Yedek Oluştur" }));
    expect(
      await screen.findByRole("button", { name: "Yedek oluşturuluyor..." }),
    ).toBeDisabled();
    fireEvent.click(
      screen.getByRole("button", { name: "Yedek oluşturuluyor..." }),
    );
    expect(api.createManualBackup).toHaveBeenCalledTimes(1);
    await act(async () => {
      resolveBackup?.({ backupPath: "safe.sqlite" });
    });
    expect(
      await screen.findByText("Yedek başarıyla oluşturuldu."),
    ).toBeInTheDocument();
  });

  it("requires confirmation before restoring the selected backup and refreshes local data", async () => {
    await openSettings();
    fireEvent.click(
      screen.getByRole("button", { name: "Yedekten Geri Yükle" }),
    );
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(api.restoreDatabaseBackup).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Vazgeç" }));
    expect(api.restoreDatabaseBackup).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByRole("button", { name: "Yedekten Geri Yükle" }),
    );
    fireEvent.click(await screen.findByRole("button", { name: "Geri Yükle" }));
    await waitFor(() =>
      expect(api.restoreDatabaseBackup).toHaveBeenCalledWith(
        "C:/selected/backup.sqlite",
      ),
    );
    expect(
      await screen.findByText("Yedek başarıyla geri yüklendi."),
    ).toBeInTheDocument();
  });

  it.each([
    [
      "RESTORE_CANDIDATE_INVALID",
      "Seçilen dosya geçerli bir BeautySaloon yedeği değil.",
    ],
    [
      "RESTORE_CANDIDATE_SCHEMA_UNSUPPORTED",
      "Bu yedek daha yeni bir BeautySaloon sürümüyle oluşturulmuş.",
    ],
    [
      "RESTORE_SAFETY_BACKUP_FAILED",
      "Güvenlik yedeği oluşturulamadığı için geri yükleme başlatılmadı.",
    ],
    [
      "RESTORE_ROLLBACK_SUCCEEDED",
      "Geri yükleme tamamlanamadı. Mevcut verileriniz korunarak işlem geri alındı.",
    ],
  ])("maps %s to a safe restore message", async (code, message) => {
    api.restoreDatabaseBackup.mockRejectedValueOnce(new Error(code));
    await openSettings();
    fireEvent.click(
      screen.getByRole("button", { name: "Yedekten Geri Yükle" }),
    );
    fireEvent.click(await screen.findByRole("button", { name: "Geri Yükle" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(message);
  });

  it("opens only the canonical data folder through its named command", async () => {
    await openSettings();
    fireEvent.click(screen.getByRole("button", { name: "Veri Klasörünü Aç" }));
    await waitFor(() => expect(api.openDataFolder).toHaveBeenCalledTimes(1));
    expect(
      screen.queryByRole("textbox", { name: /klasör|path|dosya yolu/i }),
    ).not.toBeInTheDocument();
  });
});

describe("Google and WhatsApp user settings", () => {
  const profile = {
    businessName: "Guzel Salon",
    phone: null,
    email: null,
    address: null,
    currencyCode: "TRY",
    themeKey: "default",
    logoData: null,
    logoMimeType: null,
    updatedAt: "2026-09-02T10:00:00Z",
  };

  beforeEach(() => {
    vi.clearAllMocks();
    api.listAppointmentsByDate.mockResolvedValue([]);
    api.getBusinessProfile.mockResolvedValue(profile);
    api.getGoogleCalendarStatus.mockResolvedValue({
      configured: true,
      connected: false,
      pendingSyncCount: 0,
      blockedSyncCount: 0,
    });
    api.connectGoogleCalendar.mockResolvedValue({
      connected: true,
      calendarId: "primary",
    });
    api.disconnectGoogleCalendar.mockResolvedValue(true);
    api.getCloudConnectionStatus.mockResolvedValue({
      configured: true,
      sessionPresent: false,
    });
    api.requestCloudOtp.mockResolvedValue(true);
    api.verifyCloudOtp.mockResolvedValue({
      configured: true,
      sessionPresent: true,
    });
  });

  async function openSettings(): Promise<void> {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Ayarlar" }));
    await screen.findByRole("region", { name: "Ayarlar" });
    await screen.findByText("Google Takvim bağlı değil.");
  }

  it("renders disconnected integrations without technical credentials", async () => {
    await openSettings();
    expect(
      screen.getByRole("button", { name: "Google Takvim'e Bağlan" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Hatırlatma e-postası")).toBeInTheDocument();
    expect(
      screen.queryByText(
        /Supabase|service_role|OAuth Client ID|OAuth Secret|access token|refresh token|dispatcher|cron|Meta token|phone number ID|SQL/i,
      ),
    ).not.toBeInTheDocument();
  });

  it("starts the typed Google connection once and refreshes to connected", async () => {
    let resolveConnect: ((value: unknown) => void) | undefined;
    api.connectGoogleCalendar.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveConnect = resolve;
        }),
    );
    api.getGoogleCalendarStatus
      .mockResolvedValueOnce({
        configured: true,
        connected: false,
        pendingSyncCount: 0,
        blockedSyncCount: 0,
      })
      .mockResolvedValueOnce({
        configured: true,
        connected: true,
        pendingSyncCount: 0,
        blockedSyncCount: 0,
      });
    await openSettings();
    fireEvent.click(
      screen.getByRole("button", { name: "Google Takvim'e Bağlan" }),
    );
    expect(
      await screen.findByRole("button", { name: "Bağlantı başlatılıyor..." }),
    ).toBeDisabled();
    fireEvent.click(
      screen.getByRole("button", { name: "Bağlantı başlatılıyor..." }),
    );
    expect(api.connectGoogleCalendar).toHaveBeenCalledTimes(1);
    await act(async () => {
      resolveConnect?.({ connected: true, calendarId: "primary" });
    });
    expect(
      await screen.findByText("Google Takvim — Bağlı ✓"),
    ).toBeInTheDocument();
  });

  it("shows an already persisted Google connection and can disconnect locally", async () => {
    api.getGoogleCalendarStatus.mockResolvedValue({
      configured: true,
      connected: true,
      pendingSyncCount: 0,
      blockedSyncCount: 0,
    });
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Ayarlar" }));
    expect(
      await screen.findByText("Google Takvim — Bağlı ✓"),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Bağlantıyı Kes" }));
    await waitFor(() =>
      expect(api.disconnectGoogleCalendar).toHaveBeenCalledTimes(1),
    );
  });

  it("keeps the settings usable after a safe Google failure", async () => {
    api.connectGoogleCalendar.mockRejectedValueOnce(new Error("failure"));
    await openSettings();
    fireEvent.click(
      screen.getByRole("button", { name: "Google Takvim'e Bağlan" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Google bağlantısı tamamlanamadı.",
    );
    expect(screen.getByLabelText("Hatırlatma e-postası")).toBeInTheDocument();
  });

  it("explains missing installed Google configuration without exposing OAuth details", async () => {
    api.connectGoogleCalendar.mockRejectedValueOnce(
      "VALIDATION_ERROR: GOOGLE_CONFIG_MISSING",
    );
    await openSettings();
    fireEvent.click(
      screen.getByRole("button", { name: "Google Takvim'e Bağlan" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Google Takvim bağlantısı bu bilgisayarda henüz yapılandırılmamış.",
    );
  });

  it("shows a connected-but-not-ready reminder state and saves the automatic toggle", async () => {
    api.getCloudConnectionStatus.mockResolvedValue({
      configured: true,
      sessionPresent: true,
    });
    api.getReminderReadiness.mockResolvedValue({
      state: "connected_not_ready",
      automaticEnabled: false,
    });
    api.setReminderAutomaticEnabled.mockResolvedValue({
      state: "connected_not_ready",
      automaticEnabled: true,
    });
    await openSettings();
    expect(
      await screen.findByText("Bağlı, ancak hatırlatmalar hazır değil"),
    ).toBeInTheDocument();
    fireEvent.click(
      screen.getByLabelText("Otomatik WhatsApp hatırlatmaları"),
    );
    await waitFor(() =>
      expect(api.setReminderAutomaticEnabled).toHaveBeenCalledWith(true),
    );
  });

  it("requests and verifies an email code through typed commands without storing tokens", async () => {
    api.getCloudConnectionStatus
      .mockResolvedValueOnce({ configured: true, sessionPresent: false })
      .mockResolvedValueOnce({ configured: true, sessionPresent: true });
    await openSettings();
    fireEvent.change(screen.getByLabelText("Hatırlatma e-postası"), {
      target: { value: "owner@example.com" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Kod Gönder" }));
    await waitFor(() =>
      expect(api.requestCloudOtp).toHaveBeenCalledWith("owner@example.com"),
    );
    expect(await screen.findByLabelText("Doğrulama kodu")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Doğrulama kodu"), {
      target: { value: "123456" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Doğrula" }));
    await waitFor(() =>
      expect(api.verifyCloudOtp).toHaveBeenCalledWith(
        "owner@example.com",
        "123456",
      ),
    );
    expect(
      await screen.findByText("Bağlı, ancak hatırlatmalar hazır değil"),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("Doğrulama kodu")).not.toBeInTheDocument();
  });

  it("prevents duplicate email code requests while a request is pending", async () => {
    let resolveRequest: ((value: unknown) => void) | undefined;
    api.requestCloudOtp.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveRequest = resolve;
        }),
    );
    await openSettings();
    fireEvent.change(screen.getByLabelText("Hatırlatma e-postası"), {
      target: { value: "owner@example.com" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Kod Gönder" }));
    expect(
      await screen.findByRole("button", { name: "Kod gönderiliyor..." }),
    ).toBeDisabled();
    fireEvent.click(
      screen.getByRole("button", { name: "Kod gönderiliyor..." }),
    );
    expect(api.requestCloudOtp).toHaveBeenCalledTimes(1);
    await act(async () => {
      resolveRequest?.(true);
    });
    expect(await screen.findByLabelText("Doğrulama kodu")).toBeInTheDocument();
  });

  it("shows a persisted reminder session and maps invalid codes safely", async () => {
    api.getCloudConnectionStatus.mockResolvedValue({
      configured: true,
      sessionPresent: true,
    });
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Ayarlar" }));
    expect(
      await screen.findByText("Bağlı, ancak hatırlatmalar hazır değil"),
    ).toBeInTheDocument();
    expect(
      screen.queryByLabelText("Hatırlatma e-postası"),
    ).not.toBeInTheDocument();

    api.getCloudConnectionStatus.mockResolvedValue({
      configured: true,
      sessionPresent: false,
    });
    api.verifyCloudOtp.mockRejectedValueOnce(new Error("invalid"));
    render(<App />);
    fireEvent.click(screen.getAllByRole("button", { name: "Ayarlar" }).at(-1)!);
    await screen.findByLabelText("Hatırlatma e-postası");
    fireEvent.change(screen.getAllByLabelText("Hatırlatma e-postası").at(-1)!, {
      target: { value: "owner@example.com" },
    });
    fireEvent.click(
      screen.getAllByRole("button", { name: "Kod Gönder" }).at(-1)!,
    );
    fireEvent.change(await screen.findByLabelText("Doğrulama kodu"), {
      target: { value: "000000" },
    });
    fireEvent.click(screen.getAllByRole("button", { name: "Doğrula" }).at(-1)!);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Kod doğrulanamadı. Kodu kontrol edip tekrar deneyin.",
    );
  });
});

describe("First-run onboarding", () => {
  const emptyProfile = {
    businessName: "",
    phone: null,
    email: null,
    address: null,
    currencyCode: "TRY",
    themeKey: "default",
    logoData: null,
    logoMimeType: null,
    updatedAt: "2026-09-02T10:00:00Z",
  };
  const category = {
    id: "category-1",
    name: "Genel",
    sortOrder: 1,
    isActive: true,
  };

  beforeEach(() => {
    vi.clearAllMocks();
    api.getOnboardingState.mockResolvedValue({
      needsOnboarding: true,
      nextStep: 1,
    });
    api.getBusinessProfile.mockResolvedValue(emptyProfile);
    api.updateBusinessProfile.mockResolvedValue({
      ...emptyProfile,
      businessName: "Yeni Salon",
    });
    api.createStaff.mockResolvedValue(activeStaff);
    api.listActiveStaff.mockResolvedValue([activeStaff]);
    api.listServiceCategories.mockResolvedValue([category]);
    api.createService.mockResolvedValue(activeService);
    api.setStaffServices.mockResolvedValue([activeService.id]);
    api.listAppointmentsByDate.mockResolvedValue([]);
    api.getGoogleCalendarStatus.mockResolvedValue({
      configured: false,
      connected: false,
      pendingSyncCount: 0,
      blockedSyncCount: 0,
    });
    api.getCloudConnectionStatus.mockResolvedValue({
      configured: false,
      sessionPresent: false,
    });
  });

  it("opens only for a fresh installation and keeps technical details out of the surface", async () => {
    render(<App />);
    expect(
      await screen.findByRole("main", { name: "Başlangıç kurulumu" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Onboarding işletme adı")).toBeInTheDocument();
    expect(
      screen.queryByText(/Supabase|OAuth|token|dispatcher|cron|SQL/i),
    ).not.toBeInTheDocument();
  });

  it("does not open for an existing installation, including inactive or archived history", async () => {
    api.getOnboardingState.mockResolvedValue({
      needsOnboarding: false,
      nextStep: 4,
    });
    render(<App />);
    expect(
      await screen.findByRole("heading", { name: "Bugün" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("main", { name: "Başlangıç kurulumu" }),
    ).not.toBeInTheDocument();
  });

  it("creates the required records, assigns the first service, then allows local-first completion", async () => {
    render(<App />);
    fireEvent.change(await screen.findByLabelText("Onboarding işletme adı"), {
      target: { value: "Yeni Salon" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Devam Et" }));
    await screen.findByLabelText("Onboarding personel adı");
    fireEvent.change(screen.getByLabelText("Onboarding personel adı"), {
      target: { value: "Ece" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Devam Et" }));
    await screen.findByLabelText("Onboarding hizmet adı");
    fireEvent.change(screen.getByLabelText("Onboarding hizmet adı"), {
      target: { value: "Saç Kesimi" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Devam Et" }));
    await waitFor(() =>
      expect(api.setStaffServices).toHaveBeenCalledWith("staff-1", [
        "service-1",
      ]),
    );
    expect(api.createServiceCategory).not.toHaveBeenCalled();
    fireEvent.click(
      await screen.findByRole("button", { name: "Uygulamaya Geç" }),
    );
    expect(
      await screen.findByRole("heading", { name: "Bugün" }),
    ).toBeInTheDocument();
  });

  it("resumes at the service step without creating a duplicate staff member", async () => {
    api.getOnboardingState.mockResolvedValue({
      needsOnboarding: true,
      nextStep: 3,
    });
    render(<App />);
    expect(
      await screen.findByLabelText("Onboarding hizmet adı"),
    ).toBeInTheDocument();
    expect(api.createStaff).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("Onboarding hizmet adı"), {
      target: { value: "Fön" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Devam Et" }));
    await waitFor(() => expect(api.createService).toHaveBeenCalledTimes(1));
    expect(api.createStaff).not.toHaveBeenCalled();
  });

  it("creates the fallback category only when no active category exists", async () => {
    api.listServiceCategories.mockResolvedValue([]);
    api.createServiceCategory.mockResolvedValue(category);
    api.getOnboardingState.mockResolvedValue({
      needsOnboarding: true,
      nextStep: 3,
    });
    render(<App />);
    fireEvent.change(await screen.findByLabelText("Onboarding hizmet adı"), {
      target: { value: "Fön" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Devam Et" }));
    await waitFor(() =>
      expect(api.createServiceCategory).toHaveBeenCalledWith({
        name: "Genel",
        isActive: true,
      }),
    );
    expect(api.createServiceCategory).toHaveBeenCalledTimes(1);
  });

  it("reuses the persisted fallback category when service creation is retried", async () => {
    api.listServiceCategories
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([category]);
    api.createServiceCategory.mockResolvedValue(category);
    api.createService
      .mockRejectedValueOnce(new Error("failure"))
      .mockResolvedValueOnce(activeService);
    api.getOnboardingState.mockResolvedValue({
      needsOnboarding: true,
      nextStep: 3,
    });
    render(<App />);
    fireEvent.change(await screen.findByLabelText("Onboarding hizmet adı"), {
      target: { value: "Fön" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Devam Et" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Hizmet kaydedilemedi.",
    );
    fireEvent.click(screen.getByRole("button", { name: "Devam Et" }));
    await waitFor(() => expect(api.createService).toHaveBeenCalledTimes(2));
    expect(api.createServiceCategory).toHaveBeenCalledTimes(1);
  });

  it("allows both optional connections to be skipped without a live call", async () => {
    api.getOnboardingState.mockResolvedValue({
      needsOnboarding: true,
      nextStep: 4,
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Şimdi Değil" }));
    expect(
      await screen.findByRole("heading", { name: "Bugün" }),
    ).toBeInTheDocument();
    expect(api.connectGoogleCalendar).not.toHaveBeenCalled();
    expect(api.requestCloudOtp).not.toHaveBeenCalled();
  });

  it("does not let an optional connection failure block local-first completion", async () => {
    api.getOnboardingState.mockResolvedValue({
      needsOnboarding: true,
      nextStep: 4,
    });
    api.connectGoogleCalendar.mockRejectedValueOnce(new Error("failure"));
    render(<App />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Google Takvim'e Bağlan" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Google bağlantısı tamamlanamadı.",
    );
    fireEvent.click(screen.getByRole("button", { name: "Uygulamaya Geç" }));
    expect(
      await screen.findByRole("heading", { name: "Bugün" }),
    ).toBeInTheDocument();
  });
});

describe("Service statistics and revenue", () => {
  const statisticsRows = [
    {
      serviceId: "inactive-service",
      serviceNameSnapshot: "Geçmiş Saç Kesimi",
      completedCount: 2,
      revenueMinor: 240000,
    },
    {
      serviceId: "service-3",
      serviceNameSnapshot: "Geçmiş Fön",
      completedCount: 1,
      revenueMinor: 90000,
    },
  ];

  beforeEach(() => {
    vi.clearAllMocks();
    api.getOnboardingState.mockResolvedValue({
      needsOnboarding: false,
      nextStep: 4,
    });
    api.listAppointmentsByDate.mockResolvedValue([]);
    api.listServices.mockResolvedValue([activeService, inactiveService]);
    api.listServiceCategories.mockResolvedValue([]);
    api.getBusinessProfile.mockResolvedValue({
      businessName: "Guzel Salon",
      phone: null,
      email: null,
      address: null,
      currencyCode: "TRY",
      themeKey: "default",
      logoData: null,
      logoMimeType: null,
      updatedAt: "2026-09-02T10:00:00Z",
    });
    api.serviceStatistics.mockResolvedValue(statisticsRows);
  });

  async function openStatistics(): Promise<void> {
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Hizmetler" }));
    await screen.findByRole("region", { name: "Hizmetler" });
    fireEvent.click(screen.getByRole("button", { name: "İstatistikler" }));
    await screen.findByRole("region", { name: "Hizmet istatistikleri" });
  }

  it("uses one typed aggregate query for a selected month and displays historical revenue", async () => {
    await openStatistics();
    fireEvent.change(screen.getByLabelText("İstatistik ayı"), {
      target: { value: "2026-09" },
    });
    await waitFor(() =>
      expect(api.serviceStatistics).toHaveBeenLastCalledWith(
        "2026-09-01",
        "2026-09-30",
      ),
    );
    expect(await screen.findByText("Geçmiş Saç Kesimi")).toBeInTheDocument();
    expect(screen.getByText("Ciro")).toBeInTheDocument();
    expect(screen.getByText(/₺2\.400,00/)).toBeInTheDocument();
    expect(screen.queryByText(/Kâr|Profit/i)).not.toBeInTheDocument();
    expect(api.listAppointmentsByDateRange).not.toHaveBeenCalled();
  });

  it("queries an inclusive custom range and rejects an inverted range without a request", async () => {
    await openStatistics();
    fireEvent.click(screen.getByRole("button", { name: "Tarih Aralığı" }));
    fireEvent.change(screen.getByLabelText("İstatistik başlangıç tarihi"), {
      target: { value: "2026-09-10" },
    });
    fireEvent.change(screen.getByLabelText("İstatistik bitiş tarihi"), {
      target: { value: "2026-09-01" },
    });
    api.serviceStatistics.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "Göster" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Başlangıç tarihi bitiş tarihinden sonra olamaz.",
    );
    expect(api.serviceStatistics).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("İstatistik bitiş tarihi"), {
      target: { value: "2026-09-30" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Göster" }));
    await waitFor(() =>
      expect(api.serviceStatistics).toHaveBeenCalledWith(
        "2026-09-10",
        "2026-09-30",
      ),
    );
  });

  it("keeps inactive historical services visible and maps an empty aggregate result safely", async () => {
    await openStatistics();
    expect(await screen.findByText("Geçmiş Saç Kesimi")).toBeInTheDocument();
    api.serviceStatistics.mockResolvedValueOnce([]);
    fireEvent.change(screen.getByLabelText("İstatistik ayı"), {
      target: { value: "2026-08" },
    });
    expect(
      await screen.findByText("Seçilen dönemde tamamlanmış hizmet bulunmuyor."),
    ).toBeInTheDocument();
  });
});
