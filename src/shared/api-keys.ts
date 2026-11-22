// What the API keys screens say about each optional key. Tested by api-keys.test.mjs.
import type { IgdbCredentialStatus, SteamOwnedStatus } from './types'

export type KeyId = 'steamgriddb' | 'igdb' | 'steam'
/** `set`: working setup saved; `partial`: only some of the fields are saved; `unset`: nothing saved. */
export type KeyState = 'set' | 'partial' | 'unset'
export interface KeySummary { state: KeyState; label: string }

export const keyOrder: KeyId[] = ['steamgriddb', 'igdb', 'steam']

export const keyInfo: Record<KeyId, { name: string; unlocks: string }> = {
  steamgriddb: { name: 'SteamGridDB API key', unlocks: 'Community cover art for games without a cover, plus hero and logo art on the game page.' },
  igdb: { name: 'IGDB (Twitch Client ID and Secret)', unlocks: 'Descriptions, genres, developers, publishers and ratings on the game page.' },
  steam: { name: 'Steam Web API key and SteamID64', unlocks: 'Steam games you own but have not installed, listed under All.' }
}

export const sgdbSummary = (keySet: boolean): KeySummary => keySet ? { state: 'set', label: 'Key saved' } : { state: 'unset', label: 'No key set' }

export function igdbSummary(status: IgdbCredentialStatus): KeySummary {
  if (status.clientIdSet && status.clientSecretSet) return { state: 'set', label: 'Credentials saved' }
  if (status.clientIdSet) return { state: 'partial', label: 'Client Secret missing' }
  if (status.clientSecretSet) return { state: 'partial', label: 'Client ID missing' }
  return { state: 'unset', label: 'Not configured' }
}

export function steamSummary(status: SteamOwnedStatus): KeySummary {
  if (status.keySet && status.steamIdSet) return { state: 'set', label: 'Key and SteamID saved' }
  if (status.keySet) return { state: 'partial', label: 'SteamID missing' }
  if (status.steamIdSet) return { state: 'partial', label: 'API key missing' }
  return { state: 'unset', label: 'Not set up · installed games only' }
}

export const pillWord = (state: KeyState) => state === 'set' ? 'Set' : state === 'partial' ? 'Incomplete' : 'Not set'
