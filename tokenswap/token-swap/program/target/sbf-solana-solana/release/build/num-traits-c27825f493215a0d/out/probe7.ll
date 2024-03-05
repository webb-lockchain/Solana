; ModuleID = 'probe7.ff1e7493f2c782ec-cgu.0'
source_filename = "probe7.ff1e7493f2c782ec-cgu.0"
target datalayout = "e-m:e-p:64:64-i64:64-n32:64-S128"
target triple = "sbf"

@alloc_f93507f8ba4b5780b14b2c2584609be0 = private unnamed_addr constant <{ [8 x i8] }> <{ [8 x i8] c"\00\00\00\00\00\00\F0?" }>, align 8
@alloc_ef0a1f828f3393ef691f2705e817091c = private unnamed_addr constant <{ [8 x i8] }> <{ [8 x i8] c"\00\00\00\00\00\00\00@" }>, align 8

; core::f64::<impl f64>::total_cmp
; Function Attrs: inlinehint nounwind
define internal i8 @"_ZN4core3f6421_$LT$impl$u20$f64$GT$9total_cmp17hcb9f437ddc4b66c7E"(ptr align 8 %self, ptr align 8 %other) unnamed_addr #0 {
start:
  %_23 = alloca double, align 8
  %_21 = alloca double, align 8
  %right = alloca i64, align 8
  %left = alloca i64, align 8
  %0 = alloca i8, align 1
  %self1 = load double, ptr %self, align 8, !noundef !1
  store double %self1, ptr %_21, align 8
  %rt = load double, ptr %_21, align 8, !noundef !1
  %_4 = bitcast double %rt to i64
  store i64 %_4, ptr %left, align 8
  %self2 = load double, ptr %other, align 8, !noundef !1
  store double %self2, ptr %_23, align 8
  %rt3 = load double, ptr %_23, align 8, !noundef !1
  %_7 = bitcast double %rt3 to i64
  store i64 %_7, ptr %right, align 8
  %_13 = load i64, ptr %left, align 8, !noundef !1
  %_12 = ashr i64 %_13, 63
  %_10 = lshr i64 %_12, 1
  %1 = load i64, ptr %left, align 8, !noundef !1
  %2 = xor i64 %1, %_10
  store i64 %2, ptr %left, align 8
  %_18 = load i64, ptr %right, align 8, !noundef !1
  %_17 = ashr i64 %_18, 63
  %_15 = lshr i64 %_17, 1
  %3 = load i64, ptr %right, align 8, !noundef !1
  %4 = xor i64 %3, %_15
  store i64 %4, ptr %right, align 8
  %_26 = load i64, ptr %left, align 8, !noundef !1
  %_27 = load i64, ptr %right, align 8, !noundef !1
  %_25 = icmp slt i64 %_26, %_27
  br i1 %_25, label %bb1, label %bb2

bb2:                                              ; preds = %start
  %_29 = load i64, ptr %left, align 8, !noundef !1
  %_30 = load i64, ptr %right, align 8, !noundef !1
  %_28 = icmp eq i64 %_29, %_30
  br i1 %_28, label %bb3, label %bb4

bb1:                                              ; preds = %start
  store i8 -1, ptr %0, align 1
  br label %bb6

bb4:                                              ; preds = %bb2
  store i8 1, ptr %0, align 1
  br label %bb5

bb3:                                              ; preds = %bb2
  store i8 0, ptr %0, align 1
  br label %bb5

bb5:                                              ; preds = %bb3, %bb4
  br label %bb6

bb6:                                              ; preds = %bb1, %bb5
  %5 = load i8, ptr %0, align 1, !range !2, !noundef !1
  ret i8 %5
}

; probe7::probe
; Function Attrs: nounwind
define hidden void @_ZN6probe75probe17hcca36bbbbccbb90fE() unnamed_addr #1 {
start:
; call core::f64::<impl f64>::total_cmp
  %_1 = call i8 @"_ZN4core3f6421_$LT$impl$u20$f64$GT$9total_cmp17hcb9f437ddc4b66c7E"(ptr align 8 @alloc_f93507f8ba4b5780b14b2c2584609be0, ptr align 8 @alloc_ef0a1f828f3393ef691f2705e817091c) #2, !range !2
  ret void
}

attributes #0 = { inlinehint nounwind "target-cpu"="generic" "target-features"="+solana" }
attributes #1 = { nounwind "target-cpu"="generic" "target-features"="+solana" }
attributes #2 = { nounwind }

!llvm.module.flags = !{!0}

!0 = !{i32 8, !"PIC Level", i32 2}
!1 = !{}
!2 = !{i8 -1, i8 2}
