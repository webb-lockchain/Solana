; ModuleID = 'probe3.fa1612b6c3791b1e-cgu.0'
source_filename = "probe3.fa1612b6c3791b1e-cgu.0"
target datalayout = "e-m:e-p:64:64-i64:64-n32:64-S128"
target triple = "sbf"

; probe3::probe
; Function Attrs: nounwind
define hidden void @_ZN6probe35probe17h9dff9b04efd5e96cE() unnamed_addr #0 {
start:
  %0 = alloca i32, align 4
  store i32 1, ptr %0, align 4
  %1 = load i32, ptr %0, align 4, !noundef !1
  ret void
}

; Function Attrs: nocallback nofree nosync nounwind speculatable willreturn memory(none)
declare hidden i32 @llvm.cttz.i32(i32, i1 immarg) #1

attributes #0 = { nounwind "target-cpu"="generic" "target-features"="+solana" }
attributes #1 = { nocallback nofree nosync nounwind speculatable willreturn memory(none) }

!llvm.module.flags = !{!0}

!0 = !{i32 8, !"PIC Level", i32 2}
!1 = !{}
