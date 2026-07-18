from manim import *
from manim.mobject.opengl.opengl_surface import OpenGLSurface


class Hybrid(OpenGLSurface, Mobject):
    """Mixed mesh/Mobject bases: the Cairo camera's isinstance dispatch
    finds the Mobject arm, so the verdict stays Unknown (silence)."""


class Good(ThreeDScene):
    def construct(self):
        # Cairo-capable 3D mobjects are ordinary VMobjects, not meshes.
        surface = Surface(lambda u, v: (u, v, 0))
        self.add(surface)
        # An unresolvable name stays Unknown: silence, never a guess.
        self.add(ThreeDVMobject())
        # Constructing a mesh without adding it never fires: the failure
        # is a display-time contract.
        unused = OpenGLSurface(lambda u, v: (u, v, 0))
        # Mixed mesh/Mobject inheritance stays Unknown: silence.
        self.add(Hybrid(lambda u, v: (u, v, 0)))
        # A plain 2D mobject is fine.
        self.add(Square())
        self.wait()
