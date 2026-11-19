// What the Accounts section and the first-time setup say and do. Tested by accounts.test.mjs.
import type { AccountStatus, SetupChoice, SetupStore, SetupStoreInfo } from './types'

/** `ok`: working; `partial`: signed in but something is missing; `off`: not signed in; `soon`: not in this version. */
export type AccountState = 'ok' | 'partial' | 'off' | 'soon'
export interface AccountLine { state: AccountState; label: string; pill: string }

/** The one-line status of a store account. */
export function accountLine(status: AccountStatus): AccountLine {
  if (!status.available) return { state: 'soon', label: 'Sign-in comes in a later version · installed games only', pill: 'Coming soon' }
  if (status.signingIn) return { state: 'partial', label: status.store === 'steam' ? 'Continue signing in to Steam in your browser' : 'Finish signing in to ' + status.name + ' in its sign-in window', pill: 'Signing in' }
  if (!status.signedIn) return { state: 'off', label: 'Not signed in · installed games only', pill: 'Not signed in' }
  if (status.mode === 'installed') return { state: 'partial', label: 'Signed in · showing installed games only', pill: 'Installed only' }
  if (status.store === 'steam' && !status.keySet) return { state: 'partial', label: 'Signed in · add a Steam Web API key to list the games you own', pill: 'Key missing' }
  if (status.last?.needsSignIn) return { state: 'partial', label: 'The saved sign-in no longer works · sign in again', pill: 'Sign in again' }
  if (status.store === 'steam') return { state: 'ok', label: 'Signed in · owned games and Steam playtime are synced', pill: 'Signed in' }
  if (status.store === 'gog') return { state: 'ok', label: 'Signed in · owned games and GOG playtime are synced', pill: 'Signed in' }
  return { state: 'ok', label: 'Signed in · owned games are synced', pill: 'Signed in' }
}

/** Whether "Sync now" can run for this account. */
export const canSync = (status: AccountStatus) => status.available && status.signedIn && status.mode === 'account' && (status.store !== 'steam' || status.keySet) && !status.signingIn

export type SetupStep = 'welcome' | 'stores' | 'choices' | 'signin' | 'folders' | 'keys' | 'done'

/** The setup's steps for the stores picked and the choices made; the sign-in step only when a sign-in was chosen. */
export function setupSteps(choices: Partial<Record<SetupStore, SetupChoice>>): SetupStep[] {
  const signIn = Object.values(choices).includes('signin')
  return ['welcome', 'stores', 'choices', ...(signIn ? ['signin' as const] : []), 'folders', 'keys', 'done']
}

/** The stores to sign in to in the setup's sign-in step, in the setup's order. */
export const signInStores = (stores: SetupStoreInfo[], choices: Partial<Record<SetupStore, SetupChoice>>): SetupStoreInfo[] =>
  stores.filter(store => store.signIn && choices[store.id] === 'signin')

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
