# What you have: Types, traits, schema, mocks, event bus — but nothing actually does anything yet. The schema has tables but no CRUD. The summarizer trait exists but no real Claude client. The pipeline isn't wired.             
                                                                          
  ## The three steps that make this a working system, in order:              
                                                                          
  ## 1. DB Repository Layer (immediate next)                                      
                                                                               
  ### The schema exists but there's zero code to save or load anything. Every other feature needs this.                                                    
    - save_patient(), get_patient(), list_patients_for_shift()                   
    - save_encounter(), get_encounters_for_patient()                             
    - save_result(), get_results_for_patient()                                   
    - save_note_sections(), get_latest_sections()
    - append_audit_entry()

  ## 2. Claude API Summarizer (highest-risk spike)

  ### The architecture explicitly calls this out as the bet everything rides on. If summarization quality is bad, nothing else matters.
    - Real ClaudeSummarizer implementing the Summarizer trait
    - ED-specific prompt template for HPI/Exam extraction
    - MDM generation prompt with 3-element structure
    - De-identification integrated before each API call

  ## 3. Pipeline Wiring (makes it a real CLI tool)

  ### Connect EventBus → state machine → DB → summarizer into a runnable flow where you can type mock transcripts at a terminal and see notes build up.

  ## My recommendation: Start with #1 (DB repository) — it's foundational,
    low-risk, and testable. Should take about a session. Then immediately tackle
  ## 2 (Claude summarizer) to validate the core value proposition.
