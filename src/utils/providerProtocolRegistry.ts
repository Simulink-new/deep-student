import registryData from '../../scripts/provider-protocol-registry.json';
import type { ApiProtocol } from '@/types';

export interface ProviderProtocolRecord {
  provider_type: string;
  allowed_protocols: ApiProtocol[];
  default_protocol: ApiProtocol;
  official?: boolean;
  supports_openai_responses?: boolean;
}

interface ProviderProtocolRegistryDocument {
  schema_version: string;
  updated_at: string;
  purpose?: string;
  providers: ProviderProtocolRecord[];
}

const OPENAI_COMPATIBLE_PROTOCOLS: ApiProtocol[] = ['openai_chat_completions', 'openai_responses'];
const raw = registryData as ProviderProtocolRegistryDocument;
const providers = raw.providers ?? [];

const normalize = (value?: string | null) => (value ?? '').trim().toLowerCase();

export const normalizeBaseUrlForProtocolRegistry = (url?: string | null) =>
  (url ?? '').trim().replace(/\/+$/, '').toLowerCase();

export const getProviderProtocolRecord = (providerType?: string | null): ProviderProtocolRecord | undefined => {
  const normalized = normalize(providerType);
  if (!normalized) return undefined;
  return providers.find((record) => record.provider_type === normalized);
};

const resolvesToOfficialOpenAi = (providerType?: string | null, baseUrl?: string | null) => {
  // 与 Rust resolves_to_official_openai 对齐：provider=openai 仅在 base_url 为空
  // （未配置，走官方默认端点）时视为官方；填了第三方域名的按第三方处理。
  const normalizedProvider = normalize(providerType);
  const normalizedBaseUrl = normalizeBaseUrlForProtocolRegistry(baseUrl);
  return normalizedBaseUrl.includes('api.openai.com')
    || (normalizedProvider === 'openai' && normalizedBaseUrl === '');
};

export const providerSupportsOpenAiResponses = (args: {
  providerType?: string | null;
  baseUrl?: string | null;
  supportsOpenAIResponses?: boolean | null;
}): boolean => {
  if (args.supportsOpenAIResponses === true) {
    // ★ 第三方代理安全守卫（对齐 Rust provider_supports_openai_responses）：
    // 有明确非 OpenAI 域名 base_url（中转站）的 Responses API 实现常不完整，
    // 会导致上游 502。忽略自动检测的支持标记，直接禁用。
    // 空 base_url（未配置）或 api.openai.com 不受影响。
    const baseUrlHasContent = Boolean(normalizeBaseUrlForProtocolRegistry(args.baseUrl));
    const isThirdParty = baseUrlHasContent && !resolvesToOfficialOpenAi(args.providerType, args.baseUrl);
    if (!isThirdParty) return true;
    return false;
  }
  if (resolvesToOfficialOpenAi(args.providerType, args.baseUrl)) return true;
  return getProviderProtocolRecord(args.providerType)?.supports_openai_responses === true;
};

export const getAllowedProtocolsForProviderType = (providerType?: string | null): ApiProtocol[] => {
  const record = getProviderProtocolRecord(providerType);
  return record?.allowed_protocols?.length ? record.allowed_protocols : OPENAI_COMPATIBLE_PROTOCOLS;
};

export const resolvePreferredProtocol = (args: {
  providerType?: string | null;
  adapter?: string | null;
  baseUrl?: string | null;
  supportsOpenAIResponses?: boolean | null;
}): ApiProtocol => {
  const normalizedAdapter = normalize(args.adapter);
  if (normalizedAdapter === 'anthropic') return 'anthropic_messages';
  if (normalizedAdapter === 'google') return 'google_generate_content';

  const allowed = getAllowedProtocolsForProviderType(args.providerType);
  if (providerSupportsOpenAiResponses(args) && allowed.includes('openai_responses')) {
    return 'openai_responses';
  }

  const record = getProviderProtocolRecord(args.providerType);
  if (record?.default_protocol && allowed.includes(record.default_protocol)) {
    return record.default_protocol;
  }

  return allowed[0] ?? 'openai_chat_completions';
};
