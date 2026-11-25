//! End-to-end tests of read marks (`shader.rs` `SLOT_READS`): a program
//! that picks a texture from the view heap at an index it computes stores
//! the frame's stamp at that slot of the marks; one that reads its texture
//! through a table does not.

use windows::Win32::Graphics::Direct3D12::*;

use super::host::{Dispatch, Host, Root, View};
use super::shader::{SLOT_READS, USER_FRAME};
use super::{HostShader, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};

/// Samples the texture at view-heap slot 2 (`s_load_dwordx8` at byte 0x40
/// of the heap, or of the table, at s0) with the sampler at entry 0 of the
/// table at s1, and stores red at lane * 4 of the raw UAV at entry 0 of the
/// table at s2.
const SAMPLES_SLOT_2: &[u32] = &[
    0xBE84_0300, // s_mov_b32 s4, s0
    0xBE85_0380, // s_mov_b32 s5, 0
    0xBE86_0301, // s_mov_b32 s6, s1
    0xBE87_0380, // s_mov_b32 s7, 0
    0xBE94_0302, // s_mov_b32 s20, s2
    0xBE95_0380, // s_mov_b32 s21, 0
    0xF40C_0802,
    0xFA00_0040, // s_load_dwordx8 s[32:39], s[4:5], 0x40
    0xF408_0203,
    0xFA00_0000, // s_load_dwordx4 s[8:11], s[6:7], 0x0
    0xF408_0A0A,
    0xFA00_0000, // s_load_dwordx4 s[40:43], s[20:21], 0x0
    0x7E04_0D00, // v_cvt_f32_u32 v2, v0
    0x7E06_0302, // v_mov_b32 v3, v2
    0x3420_0082, // v_lshlrev_b32 v16, 2, v0
    0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
    0xF09C_0F08,
    0x0048_0C02, // image_sample_lz v[12:15], v[2:3], s[32:39], s[8:11] dmask:0xf dim:2D
    0xBF8C_3F70, // s_waitcnt vmcnt(0)
    0xE070_1000,
    0x800A_0C10, // buffer_store_dword v12, v16, s[40:43], 0 offen
    0xBF81_0000, // s_endpgm
];

/// Lowers [`SAMPLES_SLOT_2`] as a wave32 compute program of 32 threads,
/// its texture picked from the heap (`heap`) or read through a table.
fn build(heap: bool) -> HostShader {
    let param = |root, user_data: u8| RootParam {
        root,
        table_offset_dwords: 0,
        sgprs: 1,
        base_register: 0,
        user_data,
        slot: user_data,
    };
    let range = |kind, dwords, user_data| UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords: 0,
        lower_bound: 0,
        stride: 0,
        dwords,
        user_data,
        slot: user_data,
    };
    let mut used = vec![range(ResourceType::Sampler, 4, 1), range(ResourceType::Uav, 4, 2)];
    if !heap {
        used.push(UsedResource {
            table_offset_dwords: 16,
            ..range(ResourceType::Srv, 8, 0)
        });
    }
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 32,
        layout: vec![
            param(if heap { RootType::ResourceHeap } else { RootType::Table }, 0),
            param(RootType::Table, 1),
            param(RootType::Table, 2),
        ],
        used,
        io: StageIo::Cs(ComputeIo {
            rsrc2: 3 << 1,
            threads: [32, 1, 1],
        }),
        code: SAMPLES_SLOT_2.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("track");
    let io = io::raise(&p, &native).expect("io::raise");
    let lowered = lower::lower(&lower::Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    })
    .expect("lower");
    let shader = HostShader {
        stage: HwStage::Cs,
        dxil: lowered.dxil,
        user_data: lowered.user_data,
        samplers: lowered.samplers,
        static_samplers: Vec::new(),
        root_cbvs: lowered.root_cbvs,
        root_srvs: lowered.root_srvs,
        root_uavs: lowered.root_uavs,
        root_indexed_views: Vec::new(),
    };
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .expect("DXIL validation failed");
    } else {
        eprintln!("no dxil.dll; validation skipped");
    }
    shader
}

#[test]
fn a_texture_picked_from_the_heap_is_marked_where_it_lies() {
    let shader = build(true);
    assert!(shader.root_uavs.contains(&SLOT_READS), "{:?}", shader.root_uavs);
    let frame = shader
        .user_data
        .iter()
        .find(|u| u.root == RootType::Other(USER_FRAME))
        .expect("the frame's stamp in the user data");
    // The heap's slots: the UAV (where both tables start), two textures.
    let first = [[1.0f32, 0.0, 0.0, 1.0]];
    let second = [[2.0f32, 0.0, 0.0, 1.0]];
    let views = [View::RawUav(32), View::Texture(1, 1, &first), View::Texture(1, 1, &second)];
    let samplers = [D3D12_SAMPLER_DESC {
        Filter: D3D12_FILTER_MIN_MAG_MIP_POINT,
        AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        MaxAnisotropy: 1,
        MaxLOD: f32::MAX,
        ..Default::default()
    }];
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ran = 0;
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) = host
            .compute_pipeline(&shader)
            .unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
        // Tables at heap index 0; the stamp 7. The marks are the only root
        // view: root parameter 1.
        let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        ud[frame.index as usize] = 7;
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud)), (1, Root::Uav(16))],
                    views: &views,
                    samplers: &samplers,
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        let red: Vec<f32> = out[0].iter().map(|&w| f32::from_bits(w)).collect();
        assert_eq!(red, vec![2.0; 32], "{label}: the texture at slot 2");
        let mut want = vec![0u32; 16];
        want[2] = 7;
        assert_eq!(out[1], want, "{label}: the marks");
        ran += 1;
    }
    eprintln!("ran on {ran} devices");
}

#[test]
fn a_texture_read_through_a_table_is_not_marked() {
    let shader = build(false);
    assert!(!shader.root_uavs.contains(&SLOT_READS), "{:?}", shader.root_uavs);
    assert!(!shader.user_data.iter().any(|u| u.root == RootType::Other(USER_FRAME)));
}
