% Parameters for mass_spring_damper_pid.mdl.
% Select this file as the init script on the Simulate tab.

% Plant: m x'' + b x' + k x = F
m = 1;      % mass (kg)
b = 10;     % damping (N s/m)
k = 20;     % spring stiffness (N/m)

% PID controller with a filtered derivative
Kp = 350;
Ki = 300;
Kd = 50;
Tf = 0.01;  % derivative filter time constant (s)
