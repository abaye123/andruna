# frameworks/base/libs/hwui: libhwui for the guest (ANDRUNA_AOSP_GRAPHICS).
#
# Android's libhwui (Android.bp libhwui_defaults, the android target) is the
# rendering library behind android.graphics and View: display lists,
# RenderNode, the Skia canvas, text through minikin, and the render thread
# with the SkiaOpenGL and SkiaVulkan pipelines. Skia is linked in
# (whole_static_libs: libskia), as on Android.
#
# Targets:
#   libhwui_headers  include directories of libhwui and the platform headers
#                    it uses (frameworks/native, Skia's private headers)
#   libhwui_core     the libhwui sources that build without libgui, libui's
#                    buffer allocation, libbinder, protobuf and statsd (see
#                    _hwui_blocked below and docs/plan/phase3/hwui.md)
#   libhwui_probe    every libhwui source (android target), only for
#                    measuring what compiles (not part of any link)

include("${CMAKE_CURRENT_LIST_DIR}/hwui_deps.cmake")

set(_h "${AOSP}/frameworks/base/libs/hwui")
set(_fn "${AOSP}/frameworks/native")
set(_sk "${AOSP}/external/skia")

add_library(libhwui_headers INTERFACE)
target_include_directories(libhwui_headers INTERFACE
  "${_h}" "${_h}/platform/android" "${_h}/apex/include" "${_h}/jni"
  "${_sk}/include/private" "${_sk}/src/core" "${_sk}/src/utils"
  "${_sk}/src/image" "${_sk}/src/gpu" "${_sk}/src/shaders"
  "${_fn}/libs/gui/include" "${_fn}/libs/ui/include" "${_fn}/libs/ui/include_types"
  "${_fn}/libs/nativedisplay/include" "${_fn}/libs/nativedisplay/include-private"
  "${_fn}/libs/math/include" "${_fn}/include"
  "${AOSP}/frameworks/base/libs/androidfw/include"
  "${AOSP}/frameworks/base/libs/hostgraphics"
  "${AOSP}/external/vulkan-headers/include"
  "${AOSP}/libnativehelper/include_jni" "${AOSP}/libnativehelper/header_only_include"
  "${AOSP}/external/googletest/googletest/include")
target_compile_definitions(libhwui_headers INTERFACE
  EGL_EGLEXT_PROTOTYPES GL_GLEXT_PROTOTYPES ATRACE_TAG=ATRACE_TAG_VIEW
  "LOG_TAG=\"HWUI\"")
target_link_libraries(libhwui_headers INTERFACE libskia libminikin libutils_full
  hwui_flags_aconfig libgui_flags_aconfig surfaceflinger_flags_aconfig text_flags_aconfig)

# libhwui_defaults: common sources, then the android target's.
set(_hwui_common
  canvas/CanvasFrontend.cpp canvas/CanvasOpBuffer.cpp canvas/CanvasOpRasterizer.cpp
  effects/StretchEffect.cpp effects/GainmapRenderer.cpp
  pipeline/skia/BackdropFilterDrawable.cpp pipeline/skia/HolePunch.cpp
  pipeline/skia/SkiaCpuPipeline.cpp pipeline/skia/SkiaDisplayList.cpp
  pipeline/skia/SkiaPipeline.cpp pipeline/skia/SkiaRecordingCanvas.cpp
  pipeline/skia/StretchMask.cpp pipeline/skia/RenderNodeDrawable.cpp
  pipeline/skia/ReorderBarrierDrawables.cpp pipeline/skia/TransformCanvas.cpp
  renderstate/RenderState.cpp renderthread/CanvasContext.cpp
  renderthread/DrawFrameTask.cpp renderthread/Frame.cpp
  renderthread/RenderEffectCapabilityQuery.cpp renderthread/RenderProxy.cpp
  renderthread/RenderTask.cpp renderthread/TimeLord.cpp
  hwui/AnimatedImageDrawable.cpp hwui/AnimatedImageThread.cpp hwui/Bitmap.cpp
  hwui/BlurDrawLooper.cpp hwui/Canvas.cpp hwui/ImageDecoder.cpp hwui/MinikinSkia.cpp
  hwui/OutOfProcessRendering.cpp hwui/MinikinUtils.cpp hwui/PaintImpl.cpp
  hwui/Typeface.cpp thread/CommonPool.cpp utils/Blur.cpp utils/Color.cpp
  utils/LinearAllocator.cpp utils/StringUtils.cpp utils/StatsUtils.cpp
  utils/TypefaceUtils.cpp utils/VectorDrawableUtils.cpp
  AnimationContext.cpp Animator.cpp AnimatorManager.cpp CanvasTransform.cpp
  ColorArea.cpp DamageAccumulator.cpp DeviceInfo.cpp FrameInfo.cpp
  FrameInfoVisualizer.cpp FrameMetricsReporter.cpp Gainmap.cpp Interpolator.cpp
  JankTracker.cpp Layer.cpp LayerUpdateQueue.cpp LightingInfo.cpp Matrix.cpp
  Mesh.cpp MemoryPolicy.cpp PathParser.cpp ProfileData.cpp Properties.cpp
  PropertyValuesAnimatorSet.cpp PropertyValuesHolder.cpp RecordingCanvas.cpp
  RenderNode.cpp RenderProperties.cpp RootRenderNode.cpp SkiaCanvas.cpp
  SkiaInterpolator.cpp Tonemapper.cpp TreeInfo.cpp VectorDrawable.cpp)
set(_hwui_android
  pipeline/skia/ATraceMemoryDump.cpp pipeline/skia/GLFunctorDrawable.cpp
  pipeline/skia/LayerDrawable.cpp pipeline/skia/PersistentGraphicsCache.cpp
  pipeline/skia/PipelineCache.cpp pipeline/skia/ShaderCache.cpp
  pipeline/skia/SkiaGpuPipeline.cpp pipeline/skia/SkiaIpcPipeline.cpp
  pipeline/skia/SkiaMemoryTracer.cpp pipeline/skia/SkiaOpenGLPipeline.cpp
  pipeline/skia/SkiaProfileRenderer.cpp pipeline/skia/SkiaVulkanPipeline.cpp
  pipeline/skia/VkFunctorDrawable.cpp pipeline/skia/VkInteropFunctorDrawable.cpp
  renderthread/CacheManager.cpp renderthread/EglManager.cpp
  renderthread/ReliableSurface.cpp renderthread/VulkanManager.cpp
  renderthread/VulkanSurface.cpp renderthread/RenderThread.cpp
  renderthread/HintSessionWrapper.cpp service/GraphicsStatsService.cpp
  utils/GLUtils.cpp utils/NdkUtils.cpp AutoBackendTextureRelease.cpp
  DeferredLayerUpdater.cpp HardwareBitmapUploader.cpp ProfileDataContainer.cpp
  Readback.cpp WebViewFunctorManager.cpp)

# HWUIProperties.sysprop (a source of libhwui_defaults): C++ accessors for
# ro.hwui.use_vulkan and ro.hwui.render_ahead, as Soong's sysprop_library.
set(_sysprop_out "${CMAKE_CURRENT_BINARY_DIR}/gen/hwui_sysprop")
add_custom_command(
  OUTPUT "${_sysprop_out}/HWUIProperties.sysprop.h" "${_sysprop_out}/HWUIProperties.sysprop.cpp"
  COMMAND "${Python3_EXECUTABLE}" "${CMAKE_CURRENT_LIST_DIR}/../tools/sysprop_cc.py"
          "${_h}/HWUIProperties.sysprop" "${_sysprop_out}"
  DEPENDS "${_h}/HWUIProperties.sysprop" "${CMAKE_CURRENT_LIST_DIR}/../tools/sysprop_cc.py"
  COMMENT "Generating HWUIProperties.sysprop accessors"
  VERBATIM)
target_include_directories(libhwui_headers INTERFACE "${_sysprop_out}")

function(hwui_target target)
  target_link_libraries(${target} PUBLIC aosp_android libhwui_headers)
  set_target_properties(${target} PROPERTIES CXX_STANDARD 23)
  target_compile_options(${target} PRIVATE -fvisibility=hidden
    "$<$<COMPILE_LANGUAGE:CXX>:SHELL:-include andruna/libcxx_compat.h>")
endfunction()

add_library(libhwui_probe OBJECT)
aosp_sources(libhwui_probe "${_h}" ${_hwui_common} ${_hwui_android})
target_sources(libhwui_probe PRIVATE "${_sysprop_out}/HWUIProperties.sysprop.cpp")
hwui_target(libhwui_probe)
