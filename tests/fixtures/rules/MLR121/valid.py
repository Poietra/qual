from manim import *


class DepthScene(ThreeDScene):
    def construct(self):
        box = Square()
        self.add(box)
        # In a 3D scene the z coordinate is meaningful.
        box.shift(OUT)
        self.wait()


class FlatScene(Scene):
    def construct(self):
        dot = Dot()
        self.add(dot)
        dot.shift(UP)
        # The working stacking API.
        dot.set_z_index(2)
        # Arithmetic on OUT is not the bare literal (conservative).
        dot.shift(2 * OUT)
        self.wait()
