"""Jordan Reyes demo persona: the people, the tags, and the company they work at.

Every name here is invented. Tidepool is a fictional Gastown startup selling
booking and payments software to small service businesses.
"""

ME = "Jordan Reyes"

# display name -> (role blurb, emails)
CAST = {
    "Jordan Reyes":     ("me",                      ["jordan@tidepool.app"]),
    "Priya Raman":      ("product manager",         ["priya@tidepool.app"]),
    "Simone Okafor":    ("engineering lead",        ["simone@tidepool.app"]),
    "Wes Tanaka":       ("iOS engineer",            ["wes@tidepool.app"]),
    "Devon Clarke":     ("content designer",        ["devon@tidepool.app"]),
    "Harriet Ng":       ("CEO",                     ["harriet@tidepool.app"]),
    "Renee Beaulieu":   ("design mentor",           ["renee.beaulieu@example.com"]),
    "Bea Lindqvist":    ("mentee, junior designer", ["bea.lindqvist@example.com"]),
    "Hannah Boyd":      ("recruiter, Northfield",   ["hannah@northfieldsearch.example.com"]),
    "Owen Mbeki":       ("senior designer candidate", []),
    "Marisol Vega":     ("research participant, salon owner", []),
    "Terrence Dube":    ("research participant, mobile detailer", []),
    "Aisha Mowat":      ("friend, ex-colleague",    ["aisha.mowat@example.com"]),
    "Theo Nkemelu":     ("friend",                  []),
    "Marcus Hale":      ("neighbour",               []),
    "Kai Reyes":        ("sibling",                 []),
}

# name -> (color, prompt_md, vocab_md)
TAGS = {
    "Design System": (
        "#3664E2",
        "Lead with what changed in a component or token, then who has to act on it. "
        "Name the component by its library name.",
        "Tidepool, Driftwood (the design system), token, variant, slot, Figma, "
        "SwiftUI, design lint",
    ),
    "User Research": (
        "#1FA382",
        "Summarise what the participant did and said, keeping their own words for "
        "anything that reads as a quote. Separate what they struggled with from what "
        "they asked for.",
        "participant, task, think-aloud, drop-off, booking flow, deposit, no-show fee",
    ),
    "Q4 Planning": (
        "#EB760F",
        "Open with the commitment that was made, then the dates and the owners. "
        "Flag anything that was left undecided.",
        "Q4, roadmap, milestone, scope cut, capacity, Tidepool",
    ),
    "Hiring": (
        "#B450CE",
        "Keep candidate evidence separate from opinion. Record the hire/no-hire "
        "signal each interviewer gave and what it rested on.",
        "loop, debrief, portfolio, craft, systems thinking, level",
    ),
    "Accessibility": (
        "#1C9AAB",
        "List findings by severity with the WCAG criterion where one applies, then "
        "the fix and its owner.",
        "WCAG, contrast ratio, focus order, VoiceOver, screen reader, tap target, "
        "reduced motion",
    ),
    "1:1": (
        "#7B5AD8",
        "Private notes. Capture commitments made to me and by me, and anything to "
        "raise next time. Leave feelings out of the action items.",
        "growth, scope, promotion, feedback, career",
    ),
    "Vendor": (
        "#E0B406",
        "Record what the product actually does versus what was claimed, the pricing "
        "as quoted, and the open questions for the follow-up.",
        "seat, contract, SOC 2, data residency, trial, procurement",
    ),
    "Mentoring": (
        "#2BA13B",
        "Summarise the advice given and the work the other person agreed to do "
        "before we meet again.",
        "portfolio, case study, craft, feedback, career ladder",
    ),
    "Personal": (
        "#DD4099",
        None,
        None,
    ),
    "Job Market": (
        "#DD3131",
        "Private. Keep the company, the level, the comp range and the next step. "
        "No commitments implied.",
        "recruiter, comp, level, equity, screen",
    ),
    # Trips and Spain 2027 file only the phone's recordings (the smbr-daisy-ios
    # `jordan` library); they are here so a synced tag is one tag.
    "Trips": (
        "#73A923",
        "Keep the dates, every booking with its price and its deadline, and who "
        "is doing what before the next step.",
        "itinerary, fare hold, open-jaw, deposit, travel insurance, check-in",
    ),
    "Spain 2027": (
        "#EB760F",
        None,
        "Madrid, La Latina, Seville, Alcázar, Granada, Alhambra, Nasrid Palaces, "
        "Barcelona, Gràcia, Montserrat, Sant Jeroni, AVE, Cordillera Travel",
    ),
}
