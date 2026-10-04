use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use pc98_madou_docho::{
    build_bundled_translation_test_image, build_external_text_development_image,
    build_fdi_release_patch, build_gameplay_text_development_image,
    build_graphic_text_development_image, build_localization_development_image,
    build_main_text_development_image, build_opening_graphic_poc_image, build_standalone_image,
    build_translation_test_image, rebuild_main_overlay, render_graphic_text_sources, set_input_dir,
    survey_inputs, validate_external_text_catalog, validate_gameplay_text_catalog,
    validate_graphic_text_catalog, validate_main_text_catalog, verify_source_path,
    write_external_text_catalog, write_gameplay_text_catalog, write_graphic_text_catalog,
    write_main_text_catalog,
};

#[derive(Parser)]
#[command(about = "PC-98 Madou Monogatari: Michikusa Ibun standalone disk builder")]
struct Cli {
    /// Directory holding the font, title artwork and translation catalogs.
    #[arg(long, global = true, value_name = "DIR", default_value = pc98_madou_docho::DEFAULT_INPUT_DIR)]
    assets: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Verify a supported Disc Station Vol. 03 Disk 1 source.
    VerifySource { source: PathBuf },
    /// Report the verified installer payload and derived official freeze fix as JSON.
    Survey { source: PathBuf },
    /// Build a standalone disk with the official freeze update applied.
    Build { source: PathBuf, output: PathBuf },
    /// Build a non-release opening-screen Hangul proof with a needs-review draft.
    BuildOpeningGraphicPoc {
        source: PathBuf,
        draft: PathBuf,
        output: PathBuf,
    },
    /// Extract the exact MAIN.OVL message map as a tracked translation catalog.
    ExtractMainText { source: PathBuf, output: PathBuf },
    /// Validate a MAIN.OVL catalog against the exact supported source.
    ValidateMainText { source: PathBuf, catalog: PathBuf },
    /// Validate a MAIN.OVL catalog and rebuild its Korean drafts.
    RebuildMainOverlay {
        source: PathBuf,
        catalog: PathBuf,
        output: PathBuf,
    },
    /// Build a non-release standalone image from tracked MAIN.OVL Korean drafts.
    BuildMainTextDevelopment {
        source: PathBuf,
        catalog: PathBuf,
        output: PathBuf,
    },
    /// Extract consumer-linked shop and enemy DAT text as a translation catalog.
    ExtractGameplayText { source: PathBuf, output: PathBuf },
    /// Validate a gameplay DAT catalog against the exact supported source.
    ValidateGameplayText { source: PathBuf, catalog: PathBuf },
    /// Build a non-release image from MAIN.OVL and gameplay DAT Korean drafts.
    BuildGameplayTextDevelopment {
        source: PathBuf,
        main_catalog: PathBuf,
        gameplay_catalog: PathBuf,
        output: PathBuf,
    },
    /// Render exact supported graphic resources for review.
    RenderGraphicTextSources {
        source: PathBuf,
        output_dir: PathBuf,
    },
    /// Extract reviewed graphic resources and their tracked translation surfaces.
    ExtractGraphicText { source: PathBuf, output: PathBuf },
    /// Validate a graphic-text catalog against the exact supported source review.
    ValidateGraphicText { source: PathBuf, catalog: PathBuf },
    /// Build a non-release image from tracked graphic-text Korean drafts.
    BuildGraphicTextDevelopment {
        source: PathBuf,
        catalog: PathBuf,
        output: PathBuf,
    },
    /// Build one non-release image from every currently reinsertable Korean draft surface.
    BuildLocalizationDevelopment {
        source: PathBuf,
        main_catalog: PathBuf,
        gameplay_catalog: PathBuf,
        opening_graphic_draft: PathBuf,
        graphic_text_catalog: PathBuf,
        external_text_catalog: PathBuf,
        output: PathBuf,
    },
    /// Build a complete non-release image for identified testers.
    BuildTranslationTestImage {
        source: PathBuf,
        output: PathBuf,
        /// Use catalogs from this directory instead of `translations/` under `--assets`.
        #[arg(long, value_name = "DIR")]
        catalog_dir: Option<PathBuf>,
    },
    /// Build the public 1.0.0 BPS for the exact supported standalone FDI.
    BuildFdiReleasePatch {
        /// Verified Disc Station Vol. 03 Disk 1 used to reproduce the localized files.
        disc_station_source: PathBuf,
        /// Exact standalone Japanese FDI to which users will apply the BPS.
        source_fdi: PathBuf,
        /// BPS output path.
        output: PathBuf,
        /// Optionally retain the private patched FDI used for runtime verification.
        #[arg(long, value_name = "FDI")]
        target_output: Option<PathBuf>,
    },
    /// Extract consumer-linked DOS support-program text as a translation catalog.
    ExtractExternalText { source: PathBuf, output: PathBuf },
    /// Validate the external-program text catalog against the supported standalone inputs.
    ValidateExternalText { source: PathBuf, catalog: PathBuf },
    /// Build a non-release image from source-file external-program Korean drafts.
    BuildExternalTextDevelopment {
        source: PathBuf,
        catalog: PathBuf,
        output: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    set_input_dir(cli.assets)?;
    match cli.command {
        Command::VerifySource { source } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&verify_source_path(&source)?)?
            );
        }
        Command::Survey { source } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&survey_inputs(&source)?)?
            );
        }
        Command::Build { source, output } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&build_standalone_image(&source, &output)?)?
            );
        }
        Command::BuildOpeningGraphicPoc {
            source,
            draft,
            output,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&build_opening_graphic_poc_image(
                    &source, &draft, &output,
                )?)?
            );
        }
        Command::ExtractMainText { source, output } => {
            let catalog = write_main_text_catalog(&source, &output)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "output": output,
                    "resource": catalog.resource,
                    "entry_count": catalog.entry_count,
                    "rewrite_site_count": catalog.rewrite_site_count,
                    "total_slot_bytes": catalog.total_slot_bytes,
                    "diagnostic_candidate_count": catalog.diagnostic_candidate_count,
                    "resolved_candidate_count": catalog.resolved_candidate_count,
                    "excluded_candidate_count": catalog.excluded_candidate_count,
                    "unresolved_candidate_count": catalog.unresolved_candidate_count,
                }))?
            );
        }
        Command::ValidateMainText { source, catalog } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&validate_main_text_catalog(&source, &catalog)?)?
            );
        }
        Command::RebuildMainOverlay {
            source,
            catalog,
            output,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&rebuild_main_overlay(&source, &catalog, &output,)?)?
            );
        }
        Command::BuildMainTextDevelopment {
            source,
            catalog,
            output,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&build_main_text_development_image(
                    &source, &catalog, &output,
                )?)?
            );
        }
        Command::ExtractGameplayText { source, output } => {
            let catalog = write_gameplay_text_catalog(&source, &output)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "output": output,
                    "resource_count": catalog.resource_count,
                    "entry_count": catalog.entry_count,
                }))?
            );
        }
        Command::ValidateGameplayText { source, catalog } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&validate_gameplay_text_catalog(&source, &catalog,)?)?
            );
        }
        Command::BuildGameplayTextDevelopment {
            source,
            main_catalog,
            gameplay_catalog,
            output,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&build_gameplay_text_development_image(
                    &source,
                    &main_catalog,
                    &gameplay_catalog,
                    &output,
                )?)?
            );
        }
        Command::RenderGraphicTextSources { source, output_dir } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&render_graphic_text_sources(&source, &output_dir,)?)?
            );
        }
        Command::ExtractGraphicText { source, output } => {
            let catalog = write_graphic_text_catalog(&source, &output)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "output": output,
                    "resource_count": catalog.resource_count,
                    "surface_count": catalog.surface_count,
                    "target_count": catalog.target_count,
                    "excluded_count": catalog.excluded_count,
                }))?
            );
        }
        Command::ValidateGraphicText { source, catalog } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&validate_graphic_text_catalog(&source, &catalog,)?)?
            );
        }
        Command::BuildGraphicTextDevelopment {
            source,
            catalog,
            output,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&build_graphic_text_development_image(
                    &source, &catalog, &output,
                )?)?
            );
        }
        Command::BuildLocalizationDevelopment {
            source,
            main_catalog,
            gameplay_catalog,
            opening_graphic_draft,
            graphic_text_catalog,
            external_text_catalog,
            output,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&build_localization_development_image(
                    &source,
                    &main_catalog,
                    &gameplay_catalog,
                    &opening_graphic_draft,
                    &graphic_text_catalog,
                    &external_text_catalog,
                    &output,
                )?)?
            );
        }
        Command::BuildTranslationTestImage {
            source,
            output,
            catalog_dir,
        } => {
            let report = match catalog_dir {
                Some(catalog_dir) => build_translation_test_image(
                    &source,
                    &catalog_dir.join("main"),
                    &catalog_dir.join("gameplay"),
                    &catalog_dir.join("opening-graphic.json"),
                    &catalog_dir.join("graphic-text.json"),
                    &catalog_dir.join("external"),
                    &output,
                )?,
                None => build_bundled_translation_test_image(&source, &output)?,
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::BuildFdiReleasePatch {
            disc_station_source,
            source_fdi,
            output,
            target_output,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&build_fdi_release_patch(
                    &disc_station_source,
                    &source_fdi,
                    &output,
                    target_output.as_deref(),
                )?)?
            );
        }
        Command::ExtractExternalText { source, output } => {
            let catalog = write_external_text_catalog(&source, &output)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "output": output,
                    "program_count": catalog.program_count,
                    "entry_count": catalog.entry_count,
                    "reference_count": catalog.reference_count,
                    "unresolved_program_count": catalog.unresolved_program_count,
                }))?
            );
        }
        Command::ValidateExternalText { source, catalog } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&validate_external_text_catalog(&source, &catalog,)?)?
            );
        }
        Command::BuildExternalTextDevelopment {
            source,
            catalog,
            output,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&build_external_text_development_image(
                    &source, &catalog, &output,
                )?)?
            );
        }
    }
    Ok(())
}
