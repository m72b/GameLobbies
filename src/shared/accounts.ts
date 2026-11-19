// What the Accounts section and the first-time setup say and do. Tested by accounts.test.mjs.
import type { AccountStatus, SetupChoice, SetupStore, SetupStoreInfo } from './types'

/** `ok`: working; `partial`: signed in but something is missing; `off`: not signed in; `soon`: not in this version. */
export type AccountState = 'ok' | 'partial' | 'off' | 'soon'
export interface AccountLine { state: AccountState; label: string; pill: string }

/** The one-line status of a store account. */
export function accountLine(status: AccountStatus): AccountLine {
  if (!status.available) return { state: 'soon', label: 'Sign-in comes in a later version · installed games only', pill: 'Coming soon' }
  if (status.signingIn) return { state: 'partial', label: 'Continue signing in to ' + status.name + ' in your browser', pill: 'Signing in' }
  if (!status.signedIn) return { state: 'off', label: 'Not signed in · installed games only', pill: 'Not signed in' }
  if (status.mode === 'installed') return { state: 'partial', label: 'Signed in · showing installed games only', pill: 'Installed only' }
  if (status.store === 'steam' && !status.keySet) return { state: 'partial', label: 'Signed in · add a Steam Web API key to list the games you own', pill: 'Key missing' }
  return { state: 'ok', label: 'Signed in · owned games and Steam playtime are synced', pill: 'Signed in' }
}

/** Whether "Sync now" can run for this account. */
export const canSync = (status: AccountStatus) => status.available && status.signedIn && status.mode === 'account' && (status.store !== 'steam' || status.keySet) && !status.signingIn

export type SetupStep = 'welcome' | 'stores' | 'choices' | 'steam' | 'folders' | 'keys' | 'done'

/** The setup's steps for the stores picked and the choices made; the Steam step only when Steam sign-in was chosen. */
export function setupSteps(choices: Partial<Record<SetupStore, SetupChoice>>): SetupStep[] {
  return ['welcome', 'stores', 'choices', ...(choices.steam === 'signin' ? ['steam' as const] : []), 'folders', 'keys', 'done']
}

/** The choice a picked store starts with: sign in where this version can, otherwise installed files only. */
export const defaultChoice = (store: SetupStoreInfo): SetupChoice => store.signIn ? 'signin' : 'installed'

/** Every store's choice for saving: picked stores keep theirs (a sign-in where it isn't offered becomes installed), the rest are skipped. */
export function choicesFor(stores: SetupStoreInfo[], picked: SetupStore[], choices: Partial<Record<SetupStore, SetupChoice>>): Partial<Record<SetupStore, SetupChoice>> {
  const out: Partial<Record<SetupStore, SetupChoice>> = {}
  for (const store of stores) {
    if (!picked.includes(store.id)) { out[store.id] = 'skip'; continue }
    const choice = choices[store.id] ?? defaultChoice(store)
    out[store.id] = choice === 'signin' && !store.signIn ? 'installed' : choice
  }
  return out
}

export const choiceLabel: Record<SetupChoice, string> = { signin: 'Sign in', installed: 'Installed files only', skip: 'Skip' }
