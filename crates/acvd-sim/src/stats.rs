//! Loadout -> `AcCtrlParam` + armament state for the server and the client predictor.

use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::part_field;
use acvd_proto::Loadout;

use crate::damage::DefenseProfile;

/// Simulation-facing stats of one AC.
#[derive(Clone, Debug)]
pub struct AcStats {
    pub ctrl: AcCtrlParam,
    pub en_max: f32,
    pub ap_max: f32,
    pub defense: DefenseProfile,
    /// Impact recoil that triggers the stagger penalty (`AcCtrlParamCalc.lua` line 337).
    pub stagger_threshold: f32,
}

impl AcStats {
    pub fn from_ctrl(ctrl: AcCtrlParam) -> Self {
        Self {
            en_max: ctrl.en_capacity.max(0.0),
            ap_max: 1.0,
            defense: DefenseProfile {
                ke: crate::damage::DefenseChannel::UNMITIGATED,
                ce: crate::damage::DefenseChannel::UNMITIGATED,
                te: crate::damage::DefenseChannel::UNMITIGATED,
            },
            stagger_threshold: ctrl.stagger_penalty_threshold,
            ctrl,
        }
    }

    /// Stats from a full design: AP is the frame sum (`0x8286a600`) and defenses come from
    /// `AcCtrlParamCalc.lua` lines 199-232, exactly like the single-player `TargetHealth`.
    pub fn from_design(design: &AcAssemblyDesignSt) -> Self {
        let mut ctrl = AcCtrlParam::calculate(design);
        if ctrl.en_capacity <= 0.0 {
            ctrl = AcCtrlParam::calculate(
                &acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN[0].data,
            );
        }
        let ap_max = [
            (design.head, 0u8),
            (design.core, 1),
            (design.arms, 2),
            (design.legs, 3),
        ]
        .into_iter()
        .map(|(id, cat)| part_field(id as i64, cat, "ap").max(0.0))
        .sum::<f32>()
        .max(1.0);
        Self {
            en_max: ctrl.en_capacity.max(0.0),
            ap_max,
            defense: DefenseProfile::from_ac(design),
            stagger_threshold: ctrl.stagger_penalty_threshold,
            ctrl,
        }
    }
}

/// Builds the full assembly design from the wire part ids. Fields the wire does not carry
/// (lower-body sub-slot ids, decals, squad number) stay at the empty design's values.
pub fn design_from_loadout(loadout: &Loadout) -> AcAssemblyDesignSt {
    let mut design = acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN[0].data;
    let p = &loadout.parts;
    design.head = p[Loadout::HEAD];
    design.core = p[Loadout::CORE];
    design.arms = p[Loadout::ARMS];
    design.legs = p[Loadout::LEGS];
    design.fcs = p[Loadout::FCS];
    design.gene = p[Loadout::GENERATOR];
    design.booster = p[Loadout::BOOSTER];
    design.bb = p[Loadout::BACK_BOOST];
    design.sb = p[Loadout::SIDE_BOOST];
    design.ob = p[Loadout::OVER_BOOST];
    design.armwep_r = p[Loadout::ARM_R];
    design.armwep_l = p[Loadout::ARM_L];
    design.back_r = p[Loadout::BACK_R];
    design.back_l = p[Loadout::BACK_L];
    design.shoulder = p[Loadout::SHOULDER];
    design.hanger_r = p[Loadout::HANGER_R];
    design.hanger_l = p[Loadout::HANGER_L];
    design.coloring_id = loadout.coloring_id;
    design.emblem_id = loadout.emblem_id;
    design
}

/// The wire loadout for a full assembly design.
pub fn loadout_from_design(design: &AcAssemblyDesignSt) -> Loadout {
    let mut parts = [0i16; Loadout::PARTS];
    parts[Loadout::HEAD] = design.head;
    parts[Loadout::CORE] = design.core;
    parts[Loadout::ARMS] = design.arms;
    parts[Loadout::LEGS] = design.legs;
    parts[Loadout::FCS] = design.fcs;
    parts[Loadout::GENERATOR] = design.gene;
    parts[Loadout::BOOSTER] = design.booster;
    parts[Loadout::BACK_BOOST] = design.bb;
    parts[Loadout::SIDE_BOOST] = design.sb;
    parts[Loadout::OVER_BOOST] = design.ob;
    parts[Loadout::ARM_R] = design.armwep_r;
    parts[Loadout::ARM_L] = design.armwep_l;
    parts[Loadout::BACK_R] = design.back_r;
    parts[Loadout::BACK_L] = design.back_l;
    parts[Loadout::SHOULDER] = design.shoulder;
    parts[Loadout::HANGER_R] = design.hanger_r;
    parts[Loadout::HANGER_L] = design.hanger_l;
    Loadout {
        parts,
        coloring_id: design.coloring_id,
        emblem_id: design.emblem_id,
    }
}

/// The stock "initial frame" design (`AC_ASSEMBLY_DESIGN_ST` id 5010) as a wire loadout.
pub fn default_loadout() -> Loadout {
    let design = acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN
        .iter()
        .find(|r| r.id == 5010)
        .map(|r| r.data)
        .unwrap_or(acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN[0].data);
    loadout_from_design(&design)
}

/// Stats for a loadout. A non-buildable loadout falls back to the neutral empty design so a
/// malformed join cannot panic the server; `AcCtrlParam::calculate` still returns finite values.
pub fn stats_from_loadout(loadout: &Loadout) -> AcStats {
    let design = design_from_loadout(loadout);
    AcStats::from_design(&design)
}
