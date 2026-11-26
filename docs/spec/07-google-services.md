# Google services

Many apps use Google Play services for sign-in, push messages, licence checks
and location. Andruna offers three tiers. The user chooses; the default is
the lowest tier that the user's actions call for.

| Tier | What runs | Default | What works |
| --- | --- | --- | --- |
| 0: none | Nothing. `GoogleApiAvailability` reports the services missing, as on a device without them | Until the user signs in to a Google account | Apps that do not need Google services |
| 1: microG | microG (Apache-2.0), shipped with Andruna as a system package | On after the user signs in | Sign in with Google, Firebase Cloud Messaging, Play licence checks, fused location |
| 2: Google's own services (experimental) | Google Play services, Google Services Framework and Play Store, downloaded by the user | Off | The Play Store, and apps that require Google's own services |

Never, in any tier: Play Integrity or SafetyNet attestation, Play Protect
certification, Play Billing, Widevine. They return Android's standard
"unavailable" results, and the management app explains why.

## Account sign-in

- Google's own sign-in page in a WebView2 window with a private profile.
  Andruna never sees the password.
- The resulting master token is stored only by sysd, encrypted with DPAPI.
- Apps get scoped tokens through `AccountManager.getAuthToken`, issued by the
  account authenticator for the calling package, identified by sysd from the
  caller's AppContainer.
- One Google account in the first release; multiple accounts later.

## Tier 1: microG

- Shipped unmodified, signed by microG's key, installed as a privileged
  system package.
- microG requires the platform to present Google's public signing
  certificate on its behalf ("signature spoofing"). Andruna does this **only**
  for a package with microG's package name **and** microG's signing
  certificate, and documents it as a decided desktop adaptation in the
  [compatibility policy](06-compatibility-policy.md). No other package can
  obtain it.
- microG updates ship with Andruna updates.

## Tier 2: Google's own services

- Enabled by the user in the management app's Google page, with a clear
  "experimental" label.
- Andruna does not distribute Google's apps. A setup wizard lets the user
  obtain them (download source to be decided, see
  [open questions](../open-questions.md)) or drop files they already have.
- Every file is verified before install: signed by Google, compatible with
  the CPU (x64 or arm64 variant), and with the API level Andruna presents.
  Anything else is refused with a reason.
- When tier 2 is on, microG is disabled; the two never run together.
- Uncertified device: a guided flow for Google's registration page for
  uncertified devices, using the device's real Android ID. Andruna never
  claims certification.
- Background updates by the Play Store run only while Andruna is running,
  on mains power and an unmetered network.

## Testing

- Tier 1: FCM delivery to a stopped app, sign-in, licence check against a
  test app with a known licence state.
- Tier 2: manual test protocol per release (sign-in, install a free app from
  the Play Store, update), since it depends on Google's servers.
