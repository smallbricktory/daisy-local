// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Terms of Service + Privacy Policy text shown in the in-app EULA gate
// (EulaGate.tsx). Mirrors the marketing site's /terms and /privacy pages.
//
// Acceptance versioning lives only in the backend: Eula::TOS_CURRENT /
// PRIVACY_CURRENT in crates/tauri-app/src/bootstrap.rs decide whether to
// re-prompt, and eula_status reports a bare bool. Bump them there when this
// text materially changes — a copy on this side is never read, so keeping
// one would let a bump here look effective while nothing re-prompts.

export const LEGAL_LAST_UPDATED = 'May 21, 2026';

export const TOS_MARKDOWN = `Daisy is a desktop application made by Small Bricktory ("the Studio," "we," "us"). It runs on your own computer. These Terms of Service (the "Terms") are the agreement between you and the Studio for your use of Daisy.

By installing, activating, or using Daisy, or by purchasing a license, you agree to these Terms and to our **Privacy Policy**. If you do not agree, please do not install or use Daisy. The app may also ask you to accept these Terms the first time you run it.

Daisy is published by **Apexis Solutions Inc.**, doing business as Small Bricktory ("Small Bricktory," "we," "us"). Daisy is a piece of software you license and run locally—not a hosted online service, and not a product with accounts or workspaces on our servers. Your recordings, transcripts, summaries, and notes live on your disk. We do not host them, store them, or receive them. For the full picture of the limited data we do receive, see our **Privacy Policy**.

## 1. License grant

Subject to these Terms and your active trial or subscription, the Studio grants you a personal, non-exclusive, non-transferable, revocable license to install and use Daisy for your own use. The license is per user and covers up to three (3) devices that you own or control. You may activate and deactivate devices to stay within that limit.

Daisy is currently available for Linux, Windows, and macOS. The software is licensed to you, not sold. We retain all rights, title, and interest in and to Daisy that are not expressly granted to you here.

Daisy's source code is published at **github.com/smallbricktory/daisy-local** under the **Elastic License 2.0**, which separately governs what you may do with the source (including a permission to build and run Daisy yourself for personal, non-commercial use). These Terms govern the official builds, license keys, and services.

## 2. Acceptable use

Except to the extent any of the following is prohibited by law that applies to you, you agree not to:

- resell, sublicense, rent, lease, share, distribute, or otherwise make Daisy or your license available to anyone else;
- reverse-engineer, decompile, or disassemble Daisy, or attempt to derive its source code;
- circumvent, disable, or tamper with license checks, device or seat activation, or any security or technical limitation in the app;
- remove or alter any proprietary notices, or use Daisy to build a competing product; or
- use Daisy for any unlawful purpose.

## 3. Your content stays yours

Everything you create or capture with Daisy — your audio recordings, transcripts, summaries, and notes — is yours. It is stored on your own devices, and syncs only between the Daisy devices you pair. We do not host, access, transmit, store, or receive any of it, and we claim no rights to it. You are responsible for backing up your own data.

## 4. Your responsibilities

**Recording, notice & consent.** You are solely responsible for knowing and complying with all laws on recording, notice, and consent in every jurisdiction where you record, and for how you use your recordings. This includes notifying or obtaining consent from the people being recorded where the law requires it. The same applies to enrolling speaker **voiceprints**: where a law (such as a biometric-privacy statute) requires notice or consent to collect or process biometric identifiers, obtaining it is your responsibility. These requirements vary by jurisdiction. Daisy is a tool; how, when, and whether you record or enroll voiceprints is entirely up to you and is your responsibility, not ours. We are not responsible or liable for your use or misuse of Daisy, including any failure to provide required notice or obtain required consent.

**Your vault passphrase.** If you protect your data with a passphrase, that passphrase is held only by you. We do not have it, cannot recover it, and cannot reset it. If you lose it, the data it protects, such as stored API keys and voiceprints, may be permanently inaccessible.

**Bring-your-own AI keys.** Daisy's own AI runs on your computer. It can use a third-party AI provider only if you supply your own API key for that provider. When you do, your use is subject to that provider's terms, and any usage charges are billed to you directly by that provider. We do not proxy your requests, mark up those charges, or act as a party to your relationship with the provider. You are responsible for your keys, their terms, and their costs.

**Health information (PHI).** Daisy can be configured to run in a **PHI-secure, HIPAA-friendly** manner: recording, transcription, and analysis can all happen on your own device, so protected health information ("PHI") need not leave it. Small Bricktory does not receive, store, transmit, or process your recordings, transcripts, or any PHI, and is **not a "business associate"** under HIPAA; we therefore do not offer, and will not enter into, Business Associate Agreements (BAAs). If you use Daisy where HIPAA applies, you (or the organization you act for) remain the covered entity or business associate and are responsible for your own risk analysis and safeguards, including protecting the files Daisy writes to your disk. To keep PHI on your device, use Daisy's on-device AI. If you choose a cloud AI provider, transcript text is sent **directly from your machine to that provider under your own account and key**, and holding any required BAA **with that provider** is your responsibility. Daisy is not "HIPAA certified" — no such certification exists for software.

**Automated analysis and AI-use laws.** Out of the box, Daisy does not infer emotions or sentiment, or profile, rank, or score people — its Analyzer runs only the prompts you choose, and its built-in coaching prompts give you feedback on how you communicated in a meeting. Because you choose, and can edit, those prompts, you are responsible for what you ask Daisy to produce and how you use the result. Some uses are restricted by law — the EU AI Act, for example, prohibits using AI to infer people's emotions in workplace or educational settings. Requirements vary by jurisdiction.

## 5. Plans & billing

Daisy comes with a 30-day free trial—no credit card required. After the trial, continued use requires a paid subscription. The available plans, terms, and prices are shown at checkout; the price that applies is the one presented to you when you subscribe.

Prices are exclusive of any taxes. You are responsible for any sales, use, value-added, or similar taxes that apply to your purchase, which may be added at checkout.

**Auto-renewal.** Subscriptions automatically renew at the end of each term—monthly or annual—at the then-current price for your plan, unless you cancel before the renewal date. By subscribing, you authorize us (through our payment processor) to charge your payment method on each renewal.

**Failed payments.** If a renewal payment fails, we may retry the charge and may suspend or end your license until payment succeeds.

**Price changes.** We may change our prices from time to time. Any price change applies to future terms only; we will give you reasonable notice before a new price takes effect, and you are free to cancel before your next renewal if you do not wish to continue at the new price.

## 6. Cancellation & refunds

You can cancel anytime. When you cancel, your subscription stops renewing and your license continues through the end of the term you have already paid for; it is not cut short.

**All payments are non-refundable.** Both monthly and annual subscription fees are non-refundable, except where mandatory consumer-protection law in your country of residence grants a right to a refund or withdrawal that cannot be waived; nothing in these Terms limits those non-waivable rights. The 30-day free trial is how you evaluate Daisy before paying—please use it to make sure Daisy is right for you before you subscribe.

## 7. Suspension & termination

We may suspend or end your license if you breach these Terms—for example, by misusing the app, circumventing license or device enforcement, or using Daisy unlawfully. We may also end your license if a required payment is not made. Where it is reasonable to do so, we will give you notice and a chance to fix the problem first.

If we end your license for cause, you are not entitled to a refund of any fees already paid, subject to the mandatory consumer-protection rights described above. You may stop using Daisy and cancel at any time. Sections that by their nature should survive—including ownership, disclaimers, limitation of liability, indemnification, and governing law—continue to apply after your license ends.

## 8. What data we receive

Because Daisy runs locally, we receive personal data only around your license: when you subscribe and pay, when the app checks for an update, and when you add or remove a device. We never receive your audio, transcripts, summaries, notes, or other meeting content. The **Privacy Policy** has the full picture.

## 9. Updates & changes to the app

We may release updates that fix bugs, improve performance, or add or change features. Daisy checks for updates and notifies you when one is available; you choose when to install. We may add, change, suspend, or remove features over time, and we may stop supporting older versions or platforms. We are not obligated to provide updates, new features, or support. We will make reasonable efforts to keep the app working, but we do not guarantee that any particular feature will remain available.

## 10. Disclaimers

To the maximum extent permitted by law, Daisy is provided "as is" and "as available," without warranties of any kind, whether express, implied, or statutory, including any implied warranties of merchantability, fitness for a particular purpose, title, and non-infringement. We do not warrant that Daisy will be uninterrupted or error-free.

AI-generated transcripts and summaries may contain errors, omissions, or inaccuracies, and are not a substitute for your own review and judgment. The activation and update services are provided without any uptime guarantee and may be unavailable from time to time. You use Daisy at your own risk. Nothing in these Terms excludes or limits any warranty or right that cannot be excluded or limited under the law that applies to you; where such warranties or rights apply, the disclaimers above apply only to the extent permitted.

## 11. Limitation of liability

To the maximum extent permitted by law, the Studio will not be liable for any indirect, incidental, special, consequential, or punitive damages, or for any loss of data, profits, or goodwill, arising out of or related to your use of Daisy. Our total liability for any and all claims relating to Daisy or these Terms will not exceed the greater of (a) the amount you paid us for Daisy in the twelve (12) months before the event giving rise to the claim, or (b) CAD $100.

Some jurisdictions do not allow the exclusion or limitation of certain damages or liability. Where that is the case, the limitations above apply only to the fullest extent those jurisdictions permit.

## 12. Indemnification

To the extent permitted by law, you agree to indemnify and hold harmless the Studio and its owners and contributors from any claims, losses, liabilities, and expenses (including reasonable legal fees) arising out of your content and recordings, your compliance or non-compliance with recording, notice, and consent laws, your use of third-party AI providers, or your breach of these Terms or applicable law.

## 13. Governing law

These Terms are governed by the laws of the Province of British Columbia, Canada, and the federal laws of Canada that apply there, without regard to conflict-of-laws rules. You agree to the jurisdiction of the courts located in British Columbia, Canada for any dispute that is not otherwise resolved. If you are a consumer, this does not deprive you of the protection of the mandatory laws of your country of residence.

## 14. Changes to these Terms

We may update these Terms from time to time. When we make material changes, we will provide notice—for example, by updating the "Last updated" date above, in the app, or by other reasonable means. Your continued use of Daisy after the changes take effect means you accept the updated Terms.

## 15. Contact

Questions about these Terms? Reach the Studio at support@daisylocal.app.`;

export const PRIVACY_MARKDOWN = `Daisy is a local-first app. Your recordings, audio, transcripts, summaries, notes, and tags live on your own devices, and we never store them or see what is in them. The app talks to us only about your license and about updates.

This policy explains, in plain language, exactly what little data Small Bricktory ("the Studio," "we," "us"), legally Apexis Solutions Inc. dba Small Bricktory, receives, what stays entirely on your device, and the choices you control. It works alongside our **Terms of Service**.

## What stays on your machine

The following is never transmitted to us, under any circumstance:

- **All meeting content** — recordings, audio, transcripts, summaries, notes, and tags. These are written to your local storage and stay there.
- **Voiceprints.** Stored encrypted in your Daisy data folder. We never receive them.
- **Your third-party AI provider API keys.** If you connect a cloud AI provider, your keys are kept in an encrypted vault on your device. They are not synced to us.

Because this data lives only on your machine, you control it directly.

## What we receive

To be completely explicit, here is the exhaustive list of data that ever reaches our servers, and why.

### 1. Paid sign-up and payment

When you buy a Daisy license, we collect your **name** and **email address**, and your payment is handled by a third-party payment processor. We do not see or store your full card details. We keep a license and customer record containing your name, email, license key, and your purchase and renewal status, so we can deliver and support your license.

### 2. App update checks

The app periodically asks our server whether a newer version of Daisy exists. Like any normal network request, this can include your **IP address** and the **app version** you are running. We use this only to tell the app whether an update is available. Daisy is notify-only — it tells you when an update exists; it does not silently install anything.

### 3. Device activation and deactivation

When you activate or release a device seat, the app sends your **license key** and a **random per-device identifier** so we can enforce the 3-device limit on each license. We store those with your license, and the app checks in periodically to confirm the license is still valid.

That is the complete list. We never receive your audio, transcripts, summaries, notes, or any meeting content. The only personal information we hold is your billing and licensing details — your name, email, and license record.

## Why we're allowed to process this data

For people in the European Economic Area and the United Kingdom, data protection law asks us to name a lawful basis for the limited data we handle. As applicable, we rely on:

- **Performance of your license contract** — taking payment and providing the software you licensed, including delivering updates and supporting your license.
- **Our legitimate interests** — enforcing the per-license seat limit, keeping the service secure and free of fraud, and letting you know when an update is available, balanced against your rights.
- **Your consent** — for example, when you email us and ask us to follow up. You can withdraw consent at any time.

## AI providers

Daisy includes an AI model that runs on your own device, on iPhone 15 Pro or later and on computers with a graphics card. You can also connect a cloud provider with your own key, or a model server you run yourself.

- **On-device AI** — your meeting data stays on your machine.
- **A cloud provider with your own key** — the audio or text you send goes _directly from your machine to that provider_, under that provider's own privacy policy and terms. We do not receive a copy. We encourage you to review the privacy policy of any provider you connect.

## Other connections you control

- **Models.** When you download a transcription or AI model, your computer fetches it from the site that hosts it, which receives your IP address.
- **Calendars.** If you add a calendar feed, Daisy fetches it from your calendar provider.
- **Webhooks.** If you set up a destination, Daisy sends the meeting details you select to that address.
- **MCP clients.** If you allow an MCP client to connect, it can read the meeting data you grant it.
- **A cloud-synced data folder.** If you keep your Daisy data folder in Dropbox, iCloud Drive, or OneDrive, that service holds your meetings under its own terms.

## Your Daisy devices

Daisy runs on your computer, your phone, and your tablet. Pair them and they sync your meetings over your own network, encrypted. Those devices are one private space: your meetings move between them and reach no one else. Deleting a meeting on one device deletes it on the devices it synced to.

## Our service providers

To run Daisy and the website, we use a small number of trusted providers who process the limited data described above on our behalf, under their own terms and security practices:

- **A payment processor** to handle billing and process your payment.
- **Cloud hosting and infrastructure providers** to serve this website, run the activation and update endpoints the app talks to, store activation records, and host software downloads.
- **An email provider** to send transactional messages.

These providers act as our processors and are not permitted to use your data for their own unrelated purposes.

## What we never do

- We do not sell your personal data.
- We never receive your recordings, transcripts, summaries, or notes, so we cannot use them to train AI models, nor would we.
- We do not run advertising trackers on our website.

## Our website

Our marketing website uses only what it needs to function and to understand aggregate, non-identifying traffic. We do not use advertising or cross-site tracking cookies, and we do not build advertising profiles about you. We may use aggregated, de-identified figures — such as how widely a version has been adopted — to understand and improve Daisy.

## Data retention

On-device data (recordings, transcripts, notes, voiceprints, keys) is retained on your machine until _you_ delete it. We never hold a copy. For the limited data we do hold, we keep license and customer records for as long as your license is active and for a reasonable period afterward to meet legal, tax, and accounting obligations. Activation records are kept with your license record so we can enforce the device limit. Activation and license check-in requests record your IP address briefly to limit abuse. Update-check logs are retained only briefly. Any email you send us is kept only as long as needed to handle your request, or until you ask us to delete it.

## Your rights

Depending on where you live, you may have the right to access, correct, or delete the personal data we hold, to receive a copy in a portable form, to object to or restrict certain processing, and to withdraw consent you have given. To exercise any of these, email us at support@daisylocal.app, and we will respond within the time required by applicable law. Note that deleting your customer record may deactivate your license.

In practice, much of this is straightforward: because most of your data (recordings, transcripts, notes, voiceprints, and keys) lives only on your device, we simply never hold it, and you manage and delete it directly within the app.

## Security

Data exchanged with our servers is encrypted in transit. On your device, sensitive data such as voiceprints and your provider API keys is stored encrypted. We use reasonable technical and organizational measures to protect the limited data we hold, and we keep what we hold to a minimum precisely so there is little to protect. No method of transmission or storage is completely secure, and we cannot guarantee absolute security.

## If something goes wrong

If a breach affecting your personal data occurs, we will notify you and the relevant authorities where and as required by applicable law.

## Children

Daisy is not directed to children under the age of digital consent in their jurisdiction (for example, 16 in parts of the European Union), and is not intended for use by them. We do not knowingly collect personal information from children. If you believe a child has provided us with personal information, please contact us so we can remove it.

## International transfers

We are based in British Columbia, Canada, and Daisy is used worldwide. The limited data we hold, and the information handled by our service providers, may be processed in Canada, the United States, and elsewhere. Where required, we rely on appropriate safeguards for such transfers, such as Standard Contractual Clauses or adequacy decisions. By using Daisy, you understand that this limited data may be processed outside your country of residence.

## Changes to this policy

We may update this Privacy Policy from time to time. When we do, we will revise the "Last updated" date at the top of this page. Material changes will be made clear. Your continued use of Daisy after an update means you accept the revised policy.

## Governing law

This Privacy Policy is governed by the laws of the Province of British Columbia and the federal laws of Canada applicable therein. It works alongside our **Terms of Service**. Nothing in this policy limits or waives any non-waivable rights you have under the laws of your country of residence.

## Contact

Questions about this policy, or want to access or delete your information? Email us at support@daisylocal.app.`;
