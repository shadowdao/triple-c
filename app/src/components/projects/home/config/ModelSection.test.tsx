import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import ModelSection, {
  DEFAULT_LLAMACPP_CONFIG,
  DEFAULT_OLLAMA_CONFIG,
} from "./ModelSection";
import { CUSTOM_ENDPOINT_BACKENDS } from "../../../../lib/types";
import type { Backend, Project } from "../../../../lib/types";

const baseProject: Project = {
  id: "p1",
  name: "api-server",
  paths: [{ host_path: "/src/api", mount_name: "api" }],
  container_id: null,
  status: "stopped",
  backend: "anthropic",
  bedrock_config: null,
  ollama_config: null,
  llamacpp_config: null,
  openai_compatible_config: null,
  allow_docker_access: false,
  sandbox_mode_enabled: true,
  mission_control_enabled: false,
  auth_bridge_enabled: false,
  use_shared_auth_token: true,
  full_permissions: false,
  permission_mode: null,
  ssh_key_path: null,
  git_token: null,
  git_user_name: null,
  git_user_email: null,
  custom_env_vars: [],
  port_mappings: [],
  claude_instructions: null,
  claude_code_settings: null,
  renamed_session_names: {},
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

const TOGGLE = "Use the shared Claude token";

const save = vi.fn().mockResolvedValue(true);

function renderSection(over: Partial<Project> = {}, disabled = false) {
  return render(
    <ModelSection
      project={{ ...baseProject, ...over }}
      save={save}
      disabled={disabled}
    />,
  );
}

describe("ModelSection — shared auth token toggle", () => {
  beforeEach(() => vi.clearAllMocks());

  it("renders for the Anthropic backend", () => {
    renderSection();
    expect(screen.getByRole("switch", { name: TOGGLE })).toBeInTheDocument();
  });

  it.each<Backend>(["bedrock", "ollama", "llama_cpp", "open_ai_compatible"])(
    "is hidden for the %s backend",
    (backend) => {
      renderSection({ backend });
      expect(screen.queryByRole("switch", { name: TOGGLE })).not.toBeInTheDocument();
    },
  );

  it("defaults to on, including for data written before the field existed", () => {
    renderSection();
    expect(screen.getByRole("switch", { name: TOGGLE })).toHaveAttribute(
      "aria-checked",
      "true",
    );

    const legacy = { ...baseProject } as Partial<Project>;
    delete legacy.use_shared_auth_token;
    renderSection(legacy);
    expect(screen.getAllByRole("switch", { name: TOGGLE })[1]).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("saves the opt-out and explains the consequence", () => {
    renderSection();
    fireEvent.click(screen.getByRole("switch", { name: TOGGLE }));
    expect(save).toHaveBeenCalledWith({ use_shared_auth_token: false });

    renderSection({ use_shared_auth_token: false });
    expect(screen.getByText(/needs its own `claude login`/)).toBeInTheDocument();
  });

  it("follows the container-stopped rule like the rest of the group", () => {
    renderSection({}, true);
    expect(screen.getByRole("switch", { name: TOGGLE })).toBeDisabled();
  });
});

describe("ModelSection — llama.cpp backend", () => {
  beforeEach(() => vi.clearAllMocks());

  it("is offered as a backend choice", () => {
    renderSection();
    expect(
      screen.getByRole("option", { name: "llama.cpp" }),
    ).toBeInTheDocument();
  });

  it("seeds llama-server's default port when the backend is first chosen", () => {
    renderSection();
    fireEvent.change(screen.getByLabelText("Backend"), {
      target: { value: "llama_cpp" },
    });
    expect(save).toHaveBeenCalledWith({
      backend: "llama_cpp",
      llamacpp_config: DEFAULT_LLAMACPP_CONFIG,
    });
    expect(DEFAULT_LLAMACPP_CONFIG.base_url).toContain(":8080");
  });

  it("does not clobber an existing config when re-selected", () => {
    renderSection({
      backend: "ollama",
      llamacpp_config: {
        base_url: "http://gpu-box:9090",
        model_id: "mine",
        haiku_model_id: null,
      },
    });
    fireEvent.change(screen.getByLabelText("Backend"), {
      target: { value: "llama_cpp" },
    });
    expect(save).toHaveBeenCalledWith({ backend: "llama_cpp" });
  });

  it("saves the base URL and model on blur", () => {
    renderSection({ backend: "llama_cpp" });

    const url = screen.getByLabelText("Base URL");
    fireEvent.change(url, { target: { value: "http://gpu-box:8080" } });
    fireEvent.blur(url);
    expect(save).toHaveBeenCalledWith({
      llamacpp_config: { ...DEFAULT_LLAMACPP_CONFIG, base_url: "http://gpu-box:8080" },
    });

    const model = screen.getByLabelText("Model");
    fireEvent.change(model, { target: { value: "qwen3.5-coder-30b" } });
    fireEvent.blur(model);
    expect(save).toHaveBeenCalledWith({
      llamacpp_config: { ...DEFAULT_LLAMACPP_CONFIG, model_id: "qwen3.5-coder-30b" },
    });
  });
});

describe("ModelSection — background (haiku) model override", () => {
  beforeEach(() => vi.clearAllMocks());

  it("covers exactly the backends that point at a custom endpoint", () => {
    expect([...CUSTOM_ENDPOINT_BACKENDS]).toEqual([
      "ollama",
      "llama_cpp",
      "open_ai_compatible",
    ]);
  });

  it.each([...CUSTOM_ENDPOINT_BACKENDS])("is offered for the %s backend", (backend) => {
    renderSection({ backend });
    const field = screen.getByLabelText("Background model");
    expect(field).toBeInTheDocument();
    // Blank is the documented default — it reuses the main model.
    expect(field).toHaveValue("");
    expect(field).toHaveAttribute("placeholder", "(same as the model above)");
  });

  it.each<Backend>(["anthropic", "bedrock"])(
    "is not offered for the %s backend, which keeps Claude Code's defaults",
    (backend) => {
      renderSection({ backend });
      expect(screen.queryByLabelText("Background model")).not.toBeInTheDocument();
    },
  );

  it("saves a trimmed override, and clears it back to null when blanked", () => {
    renderSection({ backend: "ollama" });
    const field = screen.getByLabelText("Background model");

    fireEvent.change(field, { target: { value: "  qwen3.5:3b  " } });
    fireEvent.blur(field);
    expect(save).toHaveBeenCalledWith({
      ollama_config: { ...DEFAULT_OLLAMA_CONFIG, haiku_model_id: "qwen3.5:3b" },
    });

    fireEvent.change(field, { target: { value: "   " } });
    fireEvent.blur(field);
    expect(save).toHaveBeenCalledWith({
      ollama_config: { ...DEFAULT_OLLAMA_CONFIG, haiku_model_id: null },
    });
  });

  it("shows an existing override and explains what it is for", () => {
    renderSection({
      backend: "llama_cpp",
      llamacpp_config: {
        base_url: "http://host.docker.internal:8080",
        model_id: "big",
        haiku_model_id: "small",
      },
    });
    expect(screen.getByLabelText("Background model")).toHaveValue("small");
    expect(screen.getByText(/background work/i)).toBeInTheDocument();
  });
});
