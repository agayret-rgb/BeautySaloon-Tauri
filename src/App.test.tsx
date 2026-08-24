import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { App } from "./App";

vi.mock("./tauriApi", () => ({
  getAppHealth: vi.fn().mockResolvedValue({
    ok: true,
    appVersion: "0.1.0",
    schemaVersion: 1,
    databasePath: "test.db",
    message: "ok"
  }),
  listFoundationNotes: vi.fn().mockResolvedValue([]),
  createFoundationNote: vi.fn()
}));

describe("App shell", () => {
  it("renders the salon navigation and primary appointment action", async () => {
    render(<App />);

    expect(screen.getByRole("button", { name: "+ Yeni Randevu" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Bugun" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Takvim" })).toBeInTheDocument();
    expect(await screen.findByText("SQLite hazir")).toBeInTheDocument();
  });
});
