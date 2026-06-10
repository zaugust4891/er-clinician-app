use er_note_app::infrastructure::ai::claude::ClaudeSummarizer;
use er_note_app::interface::export::format_note;
use er_note_app::domain::{Disposition, NoteSections, PatientRecord};
use er_note_app::infrastructure::ai::summarizer::Summarizer;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok(); // load .env before reading any env vars
    tracing_subscriber::fmt::init();

    let summarizer = ClaudeSummarizer::new().expect(
        "ANTHROPIC_KEY not found — add it to .env or export it in your shell"
    );
    
    // --- Transcript 1: Classic chest pain ---
    let transcript_1 = r#"
        So tell me what's going on today.
        I've been having this chest pain for about two hours now.
        Where exactly? Right here in the center.
        Does it go anywhere? Yeah, down my left arm a bit.
        Scale of 1 to 10? Like an 8. It's pressure, like someone sitting on my chest.
        Any shortness of breath? A little, yeah. No nausea though.
        Have you ever had anything like this before? No never.
        Any history of heart problems? No. I take lisinopril for blood pressure.
        OK let me take a look. Heart sounds regular, no murmurs.
        Lungs clear bilaterally. Abdomen soft.
        No leg swelling. Pulses are equal.
    "#;

    let prior = NoteSections::default();

    tracing::info!("=== ENCOUNTER 1: Chest Pain ===");
    match summarizer.summarize_encounter(transcript_1, &prior, 1).await {
        Ok(sections) => {
            tracing::info!("HPI: {:?}", sections.hpi);
            tracing::info!("ROS: {:?}", sections.ros);
            tracing::info!("Exam: {:?}", sections.exam);

            // Now test encounter 2 (re-evaluation)
            let transcript_2 = r#"
                How's the pain now? It's better, maybe a 4.
                The nitro helped? Yeah, took the edge off.
                Troponin came back negative. EKG looks normal sinus.
                I want to get a chest CT to rule out PE given the shortness of breath.
                Still no swelling in the legs. Lungs still clear.
            "#;

            tracing::info!("\n=== ENCOUNTER 2: Re-evaluation ===");
            match summarizer.summarize_encounter(transcript_2, &sections, 2).await {
                Ok(updated) => {
                    tracing::info!("Updated HPI: {:?}", updated.hpi);
                    tracing::info!("Updated Exam: {:?}", updated.exam);

                    // Test MDM generation
                    tracing::info!("\n=== MDM GENERATION ===");
                    let results = "Troponin I: <0.01 (normal). \
                                    EKG: NSR, no ST changes. \
                                    CTA Chest: No PE, no aortic pathology.";
                    match summarizer.generate_mdm(&updated, results, "Discharge").await {
                        Ok(mdm) => {
                            tracing::info!("MDM generated successfully");

                            // Assemble the complete note and format it for export
                            let mut final_sections = updated.clone();
                            final_sections.mdm = Some(mdm);

                            let patient = PatientRecord::new("chest pain");
                            let disposition = Disposition::Discharge {
                                instructions: "Return to ED if chest pain recurs, worsens, or new symptoms develop. Avoid strenuous activity. Follow up as directed.".into(),
                                follow_up: Some("Cardiology within 1 week, PCP within 2-3 days".into()),
                            };

                            let formatted = format_note(
                                &patient,
                                &final_sections,
                                Some(&disposition),
                                Some(results),
                            );

                            println!("\n{}", "=".repeat(60));
                            println!("{formatted}");
                            println!("{}", "=".repeat(60));
                        }
                        Err(e) => tracing::error!("MDM failed: {e}"),
                    }
                    
                }

                Err(e) => tracing::error!("Encounter 2 failed: {e}"),
            }
        }
        Err(e) => tracing::error!("Encounter 1 failed: {e}"),
    }
}