//! Ordered resident flight-resource preparation at 80035730 and 80035C90.
//!
//! Each plan is one synchronous original callback, with one loading-screen poll
//! at its tail. Readiness belongs to the preceding 354B8 poll; these callbacks
//! have no frame budget, retry timer or additional CD read. All asset payloads
//! remain in flight_assets and addresses here are native section-relative refs.
use crate::{
    Result,
    flight_assets::{FlightAircraftAssets, FlightMissionAssets, ResidentSection},
};

pub const ADVANCE_SUBSTATE: u32 = 0x0400_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreparationKind {
    Mission,
    Aircraft,
}
impl PreparationKind {
    pub const fn original_callback(self) -> u32 {
        match self {
            Self::Mission => 0x80035730,
            Self::Aircraft => 0x80035c90,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionAddress {
    pub section: usize,
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparationCommand {
    /// Initial26E98: resolve the top-level source directory.
    ResolveResourceDirectory,
    /// 2D544: source child includes its bank metadata and samples.
    BindSoundBank { child: usize },
    /// 26E50/26D8C: complete this child's ordered TIM uploads.
    UploadTextureGroup { child: usize },
    /// Original DrawSync(0); native GPU/VRAM work must be complete here.
    SynchronizeGpu,
    /// Original 270FC stages the compressed prefix at the top of RAM before
    /// in-place expansion. Immutable owned assets do not require that copy.
    PreserveCompressedPrefix {
        until_child: usize,
        pointer_count: usize,
    },
    /// AE118: asset parser owns the decoded bytes and checked allocation offset.
    ExpandSection { section: usize },
    /// Store a section-relative native binding at the original BACC0 offset.
    BindResource {
        offset: u16,
        address: SectionAddress,
    },
    /// Store an original header word in the resident context, not a pointer.
    SetContextCount { offset: u16, count: u32 },
    /// 26E98 relocation of the directory table; encoded high-bit values remain
    /// opaque. The asset parser already supplies checked native references.
    RelocateDirectory { section: usize },
    /// 26EE0 is called for every logical slot, including aliases. Native assets
    /// retain aliasing but need to resolve each distinct record only once.
    RelocateRecords {
        section: usize,
        fields: usize,
        logical_records: usize,
    },
    /// Mission directory3 slots96 onward alias directory4 records.
    ReplaceObjectSlots {
        first: usize,
        source_section: usize,
        count: usize,
    },
    /// 5D490: eight palette uploads and section0 palette-reference patches.
    PrepareMissionPalettes,
    /// 27098 sets context+1C to the decoded allocation end.
    SetArenaEnd { allocation_bytes: usize },
    /// 26FD8(0) observes that arena end and stores it at BACC0+5C.
    RecordArenaTop {
        resource_offset: u16,
        allocation_bytes: usize,
    },
    /// Call FlightLoader::poll once. Its readiness return is ignored here;
    /// these two enclosing callbacks always return ADVANCE_SUBSTATE on success.
    AdvanceLoadingScreen,
    /// After the aircraft tail poll: renderer.phase=0, parameter=0, flags|=5.
    /// Background bytes and all other render state remain intact.
    ResetRenderStatus { flags_or: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparationPlan {
    pub kind: PreparationKind,
    pub commands: Vec<PreparationCommand>,
    /// Publish only after every command (including GPU sync/poll) succeeds.
    pub callback_result: u32,
}
fn section(sections: &[ResidentSection], index: usize) -> Result<&ResidentSection> {
    sections
        .get(index)
        .filter(|s| s.index == index)
        .ok_or_else(|| "flight preparation section identity mismatch".into())
}
fn count(sections: &[ResidentSection], index: usize) -> Result<u32> {
    let bytes = section(sections, index)?.bytes();
    Ok(u32::from_le_bytes(
        bytes
            .get(..4)
            .ok_or("flight preparation header missing")?
            .try_into()?,
    ))
}
fn record_count(sections: &[ResidentSection], index: usize, fields: usize) -> Result<usize> {
    let directory = section(sections, index)?
        .directory
        .as_ref()
        .ok_or("flight preparation directory missing")?;
    if directory.fields_per_record != fields {
        return Err("flight preparation relocation width mismatch".into());
    }
    if count(sections, index)? as usize != directory.slots.len() {
        return Err("flight preparation logical count mismatch".into());
    }
    Ok(directory.slots.len())
}
fn bind(commands: &mut Vec<PreparationCommand>, offset: u16, section: usize, relative: usize) {
    commands.push(PreparationCommand::BindResource {
        offset,
        address: SectionAddress {
            section,
            offset: relative,
        },
    });
}

/// Call after the mission read completes. Parsing/decompression validation can
/// happen eagerly; execute commands in order before advancing the dispatcher.
pub fn mission_preparation(assets: &FlightMissionAssets) -> Result<PreparationPlan> {
    use PreparationCommand as C;
    let sections = assets.sections();
    if sections.len() != 11
        || assets
            .texture_groups()
            .iter()
            .map(|g| g.section)
            .collect::<Vec<_>>()
            != [15, 14, 13, 12, 11]
    {
        return Err("unexpected mission preparation layout".into());
    }
    let mut commands = vec![C::ResolveResourceDirectory, C::BindSoundBank { child: 16 }];
    for group in assets.texture_groups() {
        commands.push(C::UploadTextureGroup {
            child: group.section,
        });
        commands.push(C::SynchronizeGpu);
    }
    commands.push(C::PreserveCompressedPrefix {
        until_child: 11,
        pointer_count: 11,
    });
    commands.extend((0..11).map(|section| C::ExpandSection { section }));
    bind(&mut commands, 0x18, 0, 0);
    commands.push(C::SetContextCount {
        offset: 0xf80,
        count: count(sections, 3)?,
    });
    commands.push(C::SetContextCount {
        offset: 0xf88,
        count: count(sections, 5)?,
    });
    for (offset, section) in [(0x1c, 1), (0x38, 8), (0x30, 6), (0x34, 7)] {
        bind(&mut commands, offset, section, 0);
    }
    for (offset, section, relative) in [
        (0x20, 2, 0),
        (0x24, 3, 4),
        (0x28, 4, 4),
        (0x2c, 5, 4),
        (0x3c, 9, 4),
        (0x40, 10, 4),
    ] {
        commands.push(C::RelocateDirectory { section });
        bind(&mut commands, offset, section, relative);
    }
    for (section, fields) in [(2, 4), (3, 6), (4, 6), (5, 4)] {
        commands.push(C::RelocateRecords {
            section,
            fields,
            logical_records: record_count(sections, section, fields)?,
        });
    }
    commands.push(C::ReplaceObjectSlots {
        first: 96,
        source_section: 4,
        count: record_count(sections, 4, 6)?,
    });
    commands.push(C::PrepareMissionPalettes);
    commands.push(C::SetArenaEnd {
        allocation_bytes: assets.resident_bytes(),
    });
    commands.push(C::AdvanceLoadingScreen);
    Ok(PreparationPlan {
        kind: PreparationKind::Mission,
        commands,
        callback_result: ADVANCE_SUBSTATE,
    })
}

/// Call after the aircraft read completes. Its render reset occurs after the
/// loading-screen poll, so the poll's draw observes the pre-reset render flags.
pub fn aircraft_preparation(assets: &FlightAircraftAssets) -> Result<PreparationPlan> {
    use PreparationCommand as C;
    let sections = assets.sections();
    if sections.len() != 2
        || assets
            .texture_groups()
            .iter()
            .map(|g| g.section)
            .collect::<Vec<_>>()
            != [2]
    {
        return Err("unexpected aircraft preparation layout".into());
    }
    let mut commands = vec![
        C::ResolveResourceDirectory,
        C::BindSoundBank { child: 3 },
        C::UploadTextureGroup { child: 2 },
        C::SynchronizeGpu,
        C::PreserveCompressedPrefix {
            until_child: 2,
            pointer_count: 2,
        },
        C::ExpandSection { section: 0 },
        C::ExpandSection { section: 1 },
        C::RelocateDirectory { section: 0 },
    ];
    bind(&mut commands, 8, 0, 4);
    commands.push(C::RelocateRecords {
        section: 0,
        fields: 6,
        logical_records: record_count(sections, 0, 6)?,
    });
    commands.push(C::SetContextCount {
        offset: 0xf84,
        count: count(sections, 0)?,
    });
    bind(&mut commands, 12, 1, 0);
    commands.push(C::SetArenaEnd {
        allocation_bytes: assets.resident_bytes(),
    });
    commands.push(C::RecordArenaTop {
        resource_offset: 0x5c,
        allocation_bytes: assets.resident_bytes(),
    });
    commands.push(C::AdvanceLoadingScreen);
    commands.push(C::ResetRenderStatus { flags_or: 5 });
    Ok(PreparationPlan {
        kind: PreparationKind::Aircraft,
        commands,
        callback_result: ADVANCE_SUBSTATE,
    })
}
