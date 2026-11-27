# art/compiler: libart-compiler (the optimizing compiler used by the JIT),
# folded into libart.so as on Android. Sources from art/compiler/Android.bp
# ("libart-compiler-defaults"); code generators for the build architecture
# only. Android ELF build (spike S1 round 3).

set(_d "${AOSP}/art/compiler")
add_library(libart_compiler STATIC)
aosp_sources(libart_compiler "${_d}"
  debug/elf_debug_writer.cc
  dex/inline_method_analyser.cc
  driver/assume_value_options.cc
  driver/compiler_options.cc
  driver/dex_compilation_unit.cc
  jit/jit_compiler.cc
  jit/jit_logger.cc
  jni/quick/calling_convention.cc
  jni/quick/jni_compiler.cc
  optimizing/block_builder.cc
  optimizing/block_namer.cc
  optimizing/bounds_check_elimination.cc
  optimizing/builder.cc
  optimizing/cha_guard_optimization.cc
  optimizing/code_generation_data.cc
  optimizing/code_generator.cc
  optimizing/code_generator_utils.cc
  optimizing/code_sinking.cc
  optimizing/constant_folding.cc
  optimizing/constructor_fence_redundancy_elimination.cc
  optimizing/control_flow_simplifier.cc
  optimizing/data_type.cc
  optimizing/dead_code_elimination.cc
  optimizing/environment_input_elimination.cc
  optimizing/escape.cc
  optimizing/graph.cc
  optimizing/graph_checker.cc
  optimizing/graph_visualizer.cc
  optimizing/gvn.cc
  optimizing/handle_cache.cc
  optimizing/induction_var_analysis.cc
  optimizing/induction_var_range.cc
  optimizing/inliner.cc
  optimizing/instruction_builder.cc
  optimizing/instruction_list.cc
  optimizing/instruction_simplifier.cc
  optimizing/intrinsic_objects.cc
  optimizing/intrinsics.cc
  optimizing/licm.cc
  optimizing/linear_order.cc
  optimizing/load_store_analysis.cc
  optimizing/load_store_elimination.cc
  optimizing/locations.cc
  optimizing/loop_analysis.cc
  optimizing/loop_information.cc
  optimizing/loop_optimization.cc
  optimizing/nodes.cc
  optimizing/nodes_vector.cc
  optimizing/optimization.cc
  optimizing/optimizing_compiler.cc
  optimizing/parallel_move_resolver.cc
  optimizing/prepare_for_register_allocation.cc
  optimizing/profiling_info_builder.cc
  optimizing/reference_type_info.cc
  optimizing/reference_type_propagation.cc
  optimizing/register_allocation_resolver.cc
  optimizing/register_allocator.cc
  optimizing/register_allocator_linear_scan.cc
  optimizing/scheduler.cc
  optimizing/sharpening.cc
  optimizing/side_effects_analysis.cc
  optimizing/ssa_builder.cc
  optimizing/ssa_liveness_analysis.cc
  optimizing/ssa_phi_elimination.cc
  optimizing/stack_map_stream.cc
  optimizing/superblock_cloner.cc
  optimizing/write_barrier_elimination.cc
  trampolines/trampoline_compiler.cc
  utils/assembler.cc
  utils/jni_macro_assembler.cc
  compiler.cc)
if(ANDRUNA_ARCH STREQUAL "x64")
  aosp_sources(libart_compiler "${_d}"
    jni/quick/x86_64/calling_convention_x86_64.cc
    optimizing/intrinsics_x86_64.cc
    optimizing/instruction_simplifier_x86_64.cc
    optimizing/code_generator_x86_64.cc
    optimizing/code_generator_vector_x86_64.cc
    utils/x86_64/assembler_x86_64.cc
    utils/x86_64/jni_macro_assembler_x86_64.cc
    utils/x86_64/managed_register_x86_64.cc)
  # Soong builds x86 and x86_64 code generators together for an x86_64
  # device (they share nodes_x86.h and the x86 simplifier).
  aosp_sources(libart_compiler "${_d}"
    jni/quick/x86/calling_convention_x86.cc
    optimizing/code_generator_x86.cc
    optimizing/code_generator_vector_x86.cc
    optimizing/intrinsics_x86.cc
    optimizing/instruction_simplifier_x86_shared.cc
    optimizing/instruction_simplifier_x86.cc
    optimizing/pc_relative_fixups_x86.cc
    optimizing/x86_memory_gen.cc
    utils/x86/assembler_x86.cc
    utils/x86/jni_macro_assembler_x86.cc
    utils/x86/managed_register_x86.cc)
  target_compile_definitions(libart_compiler PUBLIC ART_ENABLE_CODEGEN_x86 ART_ENABLE_CODEGEN_x86_64)
else()
  aosp_sources(libart_compiler "${_d}"
    jni/quick/arm64/calling_convention_arm64.cc
    optimizing/code_generator_arm64.cc
    optimizing/code_generator_vector_arm64_neon.cc
    optimizing/code_generator_vector_arm64_sve.cc
    optimizing/jit_patches_arm64.cc
    optimizing/scheduler_arm64.cc
    optimizing/instruction_simplifier_arm64.cc
    optimizing/intrinsics_arm64.cc
    optimizing/fast_compiler_arm64.cc
    utils/arm64/assembler_arm64.cc
    utils/arm64/jni_macro_assembler_arm64.cc
    utils/arm64/managed_register_arm64.cc)
  target_compile_definitions(libart_compiler PUBLIC ART_ENABLE_CODEGEN_arm64)
endif()
art_operator_out(libart_compiler art/compiler
  art/compiler/linker/linker_patch.h
  art/compiler/optimizing/graph.h
  art/compiler/optimizing/locations.h
  art/compiler/optimizing/nodes.h
  art/compiler/optimizing/optimizing_compiler_stats.h
  art/compiler/optimizing/physical_register_type.h
  art/compiler/utils/arm/constants_arm.h)
target_include_directories(libart_compiler PUBLIC "${_d}" "${AOSP}/art/disassembler")
target_link_libraries(libart_compiler PUBLIC libart_runtime)
