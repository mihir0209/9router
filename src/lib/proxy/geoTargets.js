import { getSettings } from "@/lib/localDb";
import { getLowUsageCountries } from "@/lib/opencode/geoUsage";

const DEFAULT_MIN_TOKENS = 0;

/**
 * Resolve low-usage target countries for proxy acquisition.
 * Threshold comes from settings key `opencodeGeoMinTokens` (trillions,
 * exclusive) so countries with zero usage are always excluded.
 */
export async function getLowUsageCountryCodes({ limit } = {}) {
  let minTokens = DEFAULT_MIN_TOKENS;
  try {
    const settings = await getSettings();
    const configured = Number(settings?.opencodeGeoMinTokens);
    if (Number.isFinite(configured)) minTokens = configured;
  } catch {}

  const targets = await getLowUsageCountries({ minTokens, limit });
  return targets.map((target) => target.country_code);
}
