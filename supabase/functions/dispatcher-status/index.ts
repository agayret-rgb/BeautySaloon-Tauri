import { createClient } from "https://esm.sh/@supabase/supabase-js@2";

type DispatcherRow = {
  is_paused: boolean;
  pause_reason: string | null;
};

function sanitizeReason(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const safe = value.replace(/[^A-Za-z0-9_ -]/g, "_").slice(0, 96).trim();
  return safe || null;
}

Deno.serve(async (request) => {
  if (request.method !== "POST") {
    return Response.json({ ok: false, error: { code: "METHOD_NOT_ALLOWED" } }, { status: 405 });
  }

  const authorization = request.headers.get("authorization");
  if (!authorization?.startsWith("Bearer ")) {
    return Response.json({ ok: false, error: { code: "UNAUTHORIZED" } }, { status: 401 });
  }

  const url = Deno.env.get("SUPABASE_URL");
  const anonKey = Deno.env.get("SUPABASE_ANON_KEY");
  const serviceRoleKey = Deno.env.get("SUPABASE_SERVICE_ROLE_KEY");
  if (!url || !anonKey || !serviceRoleKey) {
    return Response.json({ ok: false, error: { code: "SERVICE_UNAVAILABLE" } }, { status: 503 });
  }

  const userClient = createClient(url, anonKey);
  const { data: { user }, error: userError } = await userClient.auth.getUser(authorization.slice(7));
  if (userError || !user) {
    return Response.json({ ok: false, error: { code: "UNAUTHORIZED" } }, { status: 401 });
  }

  const adminClient = createClient(url, serviceRoleKey);
  const { data, error } = await adminClient
    .from("cloud_dispatch_control")
    .select("is_paused,pause_reason")
    .eq("control_name", "whatsapp-reminder-dispatch")
    .single<DispatcherRow>();
  if (error || !data) {
    return Response.json({ ok: false, error: { code: "DISPATCHER_STATUS_UNAVAILABLE" } }, { status: 503 });
  }

  return Response.json({ paused: data.is_paused === true, pause_reason: sanitizeReason(data.pause_reason) });
});
